// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::Certificate;
use crate::PrivateKey;
use deno_core::anyhow::anyhow;
use deno_core::error::AnyError;
use deno_core::futures::future::poll_fn;
use deno_core::futures::future::Either;
use deno_core::futures::FutureExt;
use deno_core::unsync::spawn;
use rustls::ServerConfig;
use rustls_tokio_stream::ServerConfigProvider;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Debug;
use std::future::ready;
use std::future::Future;
use std::io::ErrorKind;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

type ErrorType = Rc<AnyError>;

/// A TLS certificate/private key pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsKey(pub Vec<Certificate>, pub PrivateKey);

#[derive(Clone, Debug, Default)]
pub enum TlsKeys {
  // TODO(mmastrac): We need Option<&T> for cppgc -- this is a workaround
  #[default]
  Null,
  Static(TlsKey),
  Resolver(TlsKeyResolver),
}

pub struct TlsKeysHolder(RefCell<TlsKeys>);

impl TlsKeysHolder {
  pub fn take(&self) -> TlsKeys {
    std::mem::take(&mut *self.0.borrow_mut())
  }
}

impl From<TlsKeys> for TlsKeysHolder {
  fn from(value: TlsKeys) -> Self {
    TlsKeysHolder(RefCell::new(value))
  }
}

impl TryInto<Option<TlsKey>> for TlsKeys {
  type Error = Self;
  fn try_into(self) -> Result<Option<TlsKey>, Self::Error> {
    match self {
      Self::Null => Ok(None),
      Self::Static(key) => Ok(Some(key)),
      Self::Resolver(_) => Err(self),
    }
  }
}

impl From<Option<TlsKey>> for TlsKeys {
  fn from(value: Option<TlsKey>) -> Self {
    match value {
      None => TlsKeys::Null,
      Some(key) => TlsKeys::Static(key),
    }
  }
}

enum TlsKeyState {
  Resolving(broadcast::Receiver<Result<TlsKey, ErrorType>>),
  Resolved(Result<TlsKey, ErrorType>),
}

struct TlsKeyResolverInner {
  resolution_tx: mpsc::UnboundedSender<(
    String,
    broadcast::Sender<Result<TlsKey, ErrorType>>,
  )>,
  cache: RefCell<HashMap<String, TlsKeyState>>,
}

#[derive(Clone)]
pub struct TlsKeyResolver {
  inner: Rc<TlsKeyResolverInner>,
}

impl TlsKeyResolver {
  async fn resolve_internal(
    &self,
    sni: String,
    alpn: Vec<Vec<u8>>,
  ) -> Result<Arc<ServerConfig>, AnyError> {
    let key = self.resolve(sni).await?;

    let mut tls_config = ServerConfig::builder()
      .with_safe_defaults()
      .with_no_client_auth()
      .with_single_cert(key.0, key.1)?;
    tls_config.alpn_protocols = alpn;
    Ok(tls_config.into())
  }

  pub fn into_server_config_provider(
    self,
    alpn: Vec<Vec<u8>>,
  ) -> ServerConfigProvider {
    let (tx, mut rx) = mpsc::unbounded_channel::<(_, oneshot::Sender<_>)>();

    // We don't want to make the resolver multi-threaded, but the `ServerConfigProvider` is
    // required to be wrapped in an Arc. To fix this, we spawn a task in our current runtime
    // to respond to the requests.
    spawn(async move {
      while let Some((sni, txr)) = rx.recv().await {
        _ = txr.send(self.resolve_internal(sni, alpn.clone()).await);
      }
    });

    Arc::new(move |hello| {
      // Take ownership of the SNI information
      let sni = hello.server_name().unwrap_or_default().to_owned();
      let (txr, rxr) = tokio::sync::oneshot::channel::<_>();
      _ = tx.send((sni, txr));
      rxr
        .map(|res| match res {
          Err(e) => Err(std::io::Error::new(ErrorKind::InvalidData, e)),
          Ok(Err(e)) => Err(std::io::Error::new(ErrorKind::InvalidData, e)),
          Ok(Ok(res)) => Ok(res),
        })
        .boxed()
    })
  }
}

impl Debug for TlsKeyResolver {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TlsKeyResolver").finish()
  }
}

pub fn new_resolver() -> (TlsKeyResolver, TlsKeyLookup) {
  let (resolution_tx, resolution_rx) = mpsc::unbounded_channel();
  (
    TlsKeyResolver {
      inner: Rc::new(TlsKeyResolverInner {
        resolution_tx,
        cache: Default::default(),
      }),
    },
    TlsKeyLookup {
      resolution_rx: RefCell::new(resolution_rx),
      pending: Default::default(),
    },
  )
}

impl TlsKeyResolver {
  /// Resolve the certificate and key for a given host. This immediately spawns a task in the
  /// background and is therefore cancellation-safe.
  pub fn resolve(
    &self,
    sni: String,
  ) -> impl Future<Output = Result<TlsKey, AnyError>> {
    let mut cache = self.inner.cache.borrow_mut();
    let mut recv = match cache.get(&sni) {
      None => {
        let (tx, rx) = broadcast::channel(1);
        cache.insert(sni.clone(), TlsKeyState::Resolving(rx.resubscribe()));
        _ = self.inner.resolution_tx.send((sni.clone(), tx));
        rx
      }
      Some(TlsKeyState::Resolving(recv)) => recv.resubscribe(),
      Some(TlsKeyState::Resolved(res)) => {
        return Either::Left(ready(res.clone().map_err(|_| anyhow!("Failed"))));
      }
    };
    drop(cache);

    // Make this cancellation safe
    let inner = self.inner.clone();
    let handle = spawn(async move {
      let res = recv.recv().await?;
      let mut cache = inner.cache.borrow_mut();
      match cache.get(&sni) {
        None | Some(TlsKeyState::Resolving(..)) => {
          cache.insert(sni, TlsKeyState::Resolved(res.clone()));
        }
        Some(TlsKeyState::Resolved(..)) => {
          // Someone beat us to it
        }
      }
      res.map_err(|_| anyhow!("Failed"))
    });
    Either::Right(async move { handle.await? })
  }
}

pub struct TlsKeyLookup {
  #[allow(clippy::type_complexity)]
  resolution_rx: RefCell<
    mpsc::UnboundedReceiver<(
      String,
      broadcast::Sender<Result<TlsKey, ErrorType>>,
    )>,
  >,
  pending:
    RefCell<HashMap<String, broadcast::Sender<Result<TlsKey, ErrorType>>>>,
}

impl TlsKeyLookup {
  /// Only one poll call may be active at any time. This method holds a `RefCell` lock.
  pub async fn poll(&self) -> Option<String> {
    if let Some((sni, sender)) =
      poll_fn(|cx| self.resolution_rx.borrow_mut().poll_recv(cx)).await
    {
      self.pending.borrow_mut().insert(sni.clone(), sender);
      Some(sni)
    } else {
      None
    }
  }

  /// Resolve a previously polled item.
  pub fn resolve(&self, sni: String, res: Result<TlsKey, AnyError>) {
    _ = self
      .pending
      .borrow_mut()
      .remove(&sni)
      .unwrap()
      .send(res.map_err(Rc::new));
  }
}

#[cfg(test)]
pub mod tests {
  use super::*;
  use deno_core::unsync::spawn;
  use rustls::Certificate;
  use rustls::PrivateKey;

  fn tls_key_for_test(sni: &str) -> TlsKey {
    TlsKey(
      vec![Certificate(format!("{sni}-cert").into_bytes())],
      PrivateKey(format!("{sni}-key").into_bytes()),
    )
  }

  #[tokio::test]
  async fn test_resolve_once() {
    let (resolver, lookup) = new_resolver();
    let task = spawn(async move {
      while let Some(sni) = lookup.poll().await {
        lookup.resolve(sni.clone(), Ok(tls_key_for_test(&sni)));
      }
    });

    let key = resolver.resolve("example.com".to_owned()).await.unwrap();
    assert_eq!(tls_key_for_test("example.com"), key);
    drop(resolver);

    task.await.unwrap();
  }

  #[tokio::test]
  async fn test_resolve_concurrent() {
    let (resolver, lookup) = new_resolver();
    let task = spawn(async move {
      while let Some(sni) = lookup.poll().await {
        lookup.resolve(sni.clone(), Ok(tls_key_for_test(&sni)));
      }
    });

    let f1 = resolver.resolve("example.com".to_owned());
    let f2 = resolver.resolve("example.com".to_owned());

    let key = f1.await.unwrap();
    assert_eq!(tls_key_for_test("example.com"), key);
    let key = f2.await.unwrap();
    assert_eq!(tls_key_for_test("example.com"), key);
    drop(resolver);

    task.await.unwrap();
  }

  #[tokio::test]
  async fn test_resolve_multiple_concurrent() {
    let (resolver, lookup) = new_resolver();
    let task = spawn(async move {
      while let Some(sni) = lookup.poll().await {
        lookup.resolve(sni.clone(), Ok(tls_key_for_test(&sni)));
      }
    });

    let f1 = resolver.resolve("example1.com".to_owned());
    let f2 = resolver.resolve("example2.com".to_owned());

    let key = f1.await.unwrap();
    assert_eq!(tls_key_for_test("example.com"), key);
    let key = f2.await.unwrap();
    assert_eq!(tls_key_for_test("example.com"), key);
    drop(resolver);

    task.await.unwrap();
  }
}