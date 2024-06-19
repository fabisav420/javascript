// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::collections::HashMap;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::anyhow::anyhow;
use deno_core::error::AnyError;
use deno_runtime::deno_fs::AccessCheckCb;
use deno_runtime::deno_fs::FileSystem;
use deno_runtime::deno_fs::FsDirEntry;
use deno_runtime::deno_fs::FsFileType;
use deno_runtime::deno_fs::OpenOptions;
use deno_runtime::deno_fs::RealFs;
use deno_runtime::deno_io::fs::File;
use deno_runtime::deno_io::fs::FsError;
use deno_runtime::deno_io::fs::FsResult;
use deno_runtime::deno_io::fs::FsStat;
use eszip::Module;

use super::virtual_fs::FileBackedVfs;
use super::virtual_fs::VfsRoot;

pub struct EszipFileSystemResources(HashMap<String, Module>);

impl EszipFileSystemResources {
  pub fn load(m: &eszip::EszipV2) -> Result<Self, AnyError> {
    let mut files = HashMap::new();
    for x in m
      .specifiers()
      .into_iter()
      .filter(|x| x.starts_with("internal://npm_vfs/"))
    {
      let m = m
        .get_module(&x)
        .ok_or_else(|| anyhow!("Module not found: {}", x))?;
      files.insert(x, m);
    }
    Ok(Self(files))
  }
}

#[derive(Clone)]
pub struct EszipFileSystem(Arc<Inner>);

impl Debug for EszipFileSystem {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("EszipFileSystem").finish()
  }
}

struct Inner {
  files: HashMap<String, Module>,
  root_path: PathBuf,
}

impl EszipFileSystem {
  pub fn new(data: EszipFileSystemResources, root_path: PathBuf) -> Self {
    Self(Arc::new(Inner {
      files: data.0,
      root_path,
    }))
  }

  fn is_path_within(&self, path: &Path) -> bool {
    path.starts_with(&self.0.root_path)
  }

  fn error_if_in_vfs(&self, path: &Path) -> FsResult<()> {
    if self.is_path_within(path) {
      Err(FsError::NotSupported)
    } else {
      Ok(())
    }
  }
}

#[async_trait::async_trait(?Send)]
impl FileSystem for EszipFileSystem {
  fn cwd(&self) -> FsResult<PathBuf> {
    RealFs.cwd()
  }

  fn tmp_dir(&self) -> FsResult<PathBuf> {
    RealFs.tmp_dir()
  }

  fn chdir(&self, path: &Path) -> FsResult<()> {
    self.error_if_in_vfs(path)?;
    RealFs.chdir(path)
  }

  fn umask(&self, mask: Option<u32>) -> FsResult<u32> {
    RealFs.umask(mask)
  }

  fn open_sync(
    &self,
    _path: &Path,
    _options: OpenOptions,
    _access_check: Option<AccessCheckCb>,
  ) -> FsResult<Rc<dyn File>> {
    Err(FsError::NotSupported)
  }
  async fn open_async<'a>(
    &'a self,
    path: PathBuf,
    options: OpenOptions,
    access_check: Option<AccessCheckCb<'a>>,
  ) -> FsResult<Rc<dyn File>> {
    if self.is_path_within(&path) {
      Err(FsError::NotSupported)
    } else {
      RealFs.open_async(path, options, access_check).await
    }
  }

  fn mkdir_sync(
    &self,
    path: &Path,
    recursive: bool,
    mode: u32,
  ) -> FsResult<()> {
    self.error_if_in_vfs(path)?;
    RealFs.mkdir_sync(path, recursive, mode)
  }
  async fn mkdir_async(
    &self,
    path: PathBuf,
    recursive: bool,
    mode: u32,
  ) -> FsResult<()> {
    self.error_if_in_vfs(&path)?;
    RealFs.mkdir_async(path, recursive, mode).await
  }

  fn chmod_sync(&self, path: &Path, mode: u32) -> FsResult<()> {
    self.error_if_in_vfs(path)?;
    RealFs.chmod_sync(path, mode)
  }
  async fn chmod_async(&self, path: PathBuf, mode: u32) -> FsResult<()> {
    self.error_if_in_vfs(&path)?;
    RealFs.chmod_async(path, mode).await
  }

  fn chown_sync(
    &self,
    path: &Path,
    uid: Option<u32>,
    gid: Option<u32>,
  ) -> FsResult<()> {
    self.error_if_in_vfs(path)?;
    RealFs.chown_sync(path, uid, gid)
  }
  async fn chown_async(
    &self,
    path: PathBuf,
    uid: Option<u32>,
    gid: Option<u32>,
  ) -> FsResult<()> {
    self.error_if_in_vfs(&path)?;
    RealFs.chown_async(path, uid, gid).await
  }

  fn remove_sync(&self, path: &Path, recursive: bool) -> FsResult<()> {
    self.error_if_in_vfs(path)?;
    RealFs.remove_sync(path, recursive)
  }
  async fn remove_async(&self, path: PathBuf, recursive: bool) -> FsResult<()> {
    self.error_if_in_vfs(&path)?;
    RealFs.remove_async(path, recursive).await
  }

  fn copy_file_sync(&self, oldpath: &Path, newpath: &Path) -> FsResult<()> {
    self.error_if_in_vfs(newpath)?;
    if self.is_path_within(oldpath) {
      Err(FsError::NotSupported)
    } else {
      RealFs.copy_file_sync(oldpath, newpath)
    }
  }
  async fn copy_file_async(
    &self,
    oldpath: PathBuf,
    newpath: PathBuf,
  ) -> FsResult<()> {
    self.error_if_in_vfs(&newpath)?;
    if self.is_path_within(&oldpath) {
      Err(FsError::NotSupported)
    } else {
      RealFs.copy_file_async(oldpath, newpath).await
    }
  }

  fn cp_sync(&self, from: &Path, to: &Path) -> FsResult<()> {
    self.error_if_in_vfs(to)?;

    RealFs.cp_sync(from, to)
  }
  async fn cp_async(&self, from: PathBuf, to: PathBuf) -> FsResult<()> {
    self.error_if_in_vfs(&to)?;

    RealFs.cp_async(from, to).await
  }

  fn stat_sync(&self, path: &Path) -> FsResult<FsStat> {
    if self.is_path_within(path) {
      Err(FsError::NotSupported)
    } else {
      RealFs.stat_sync(path)
    }
  }
  async fn stat_async(&self, path: PathBuf) -> FsResult<FsStat> {
    if self.is_path_within(&path) {
      Err(FsError::NotSupported)
    } else {
      RealFs.stat_async(path).await
    }
  }

  fn lstat_sync(&self, path: &Path) -> FsResult<FsStat> {
    if self.is_path_within(path) {
      Err(FsError::NotSupported)
    } else {
      RealFs.lstat_sync(path)
    }
  }
  async fn lstat_async(&self, path: PathBuf) -> FsResult<FsStat> {
    if self.is_path_within(&path) {
      Err(FsError::NotSupported)
    } else {
      RealFs.lstat_async(path).await
    }
  }

  fn realpath_sync(&self, path: &Path) -> FsResult<PathBuf> {
    if self.is_path_within(path) {
      Ok(path.to_path_buf())
    } else {
      RealFs.realpath_sync(path)
    }
  }
  async fn realpath_async(&self, path: PathBuf) -> FsResult<PathBuf> {
    if self.is_path_within(&path) {
      Ok(path.to_path_buf())
    } else {
      RealFs.realpath_async(path).await
    }
  }

  fn read_dir_sync(&self, path: &Path) -> FsResult<Vec<FsDirEntry>> {
    if self.is_path_within(path) {
      Err(FsError::NotSupported)
    } else {
      RealFs.read_dir_sync(path)
    }
  }
  async fn read_dir_async(&self, path: PathBuf) -> FsResult<Vec<FsDirEntry>> {
    if self.is_path_within(&path) {
      Err(FsError::NotSupported)
    } else {
      RealFs.read_dir_async(path).await
    }
  }

  fn rename_sync(&self, oldpath: &Path, newpath: &Path) -> FsResult<()> {
    self.error_if_in_vfs(oldpath)?;
    self.error_if_in_vfs(newpath)?;
    RealFs.rename_sync(oldpath, newpath)
  }
  async fn rename_async(
    &self,
    oldpath: PathBuf,
    newpath: PathBuf,
  ) -> FsResult<()> {
    self.error_if_in_vfs(&oldpath)?;
    self.error_if_in_vfs(&newpath)?;
    RealFs.rename_async(oldpath, newpath).await
  }

  fn link_sync(&self, oldpath: &Path, newpath: &Path) -> FsResult<()> {
    self.error_if_in_vfs(oldpath)?;
    self.error_if_in_vfs(newpath)?;
    RealFs.link_sync(oldpath, newpath)
  }
  async fn link_async(
    &self,
    oldpath: PathBuf,
    newpath: PathBuf,
  ) -> FsResult<()> {
    self.error_if_in_vfs(&oldpath)?;
    self.error_if_in_vfs(&newpath)?;
    RealFs.link_async(oldpath, newpath).await
  }

  fn symlink_sync(
    &self,
    oldpath: &Path,
    newpath: &Path,
    file_type: Option<FsFileType>,
  ) -> FsResult<()> {
    self.error_if_in_vfs(oldpath)?;
    self.error_if_in_vfs(newpath)?;
    RealFs.symlink_sync(oldpath, newpath, file_type)
  }
  async fn symlink_async(
    &self,
    oldpath: PathBuf,
    newpath: PathBuf,
    file_type: Option<FsFileType>,
  ) -> FsResult<()> {
    self.error_if_in_vfs(&oldpath)?;
    self.error_if_in_vfs(&newpath)?;
    RealFs.symlink_async(oldpath, newpath, file_type).await
  }

  fn read_link_sync(&self, path: &Path) -> FsResult<PathBuf> {
    if self.is_path_within(path) {
      Err(FsError::NotSupported)
    } else {
      RealFs.read_link_sync(path)
    }
  }
  async fn read_link_async(&self, path: PathBuf) -> FsResult<PathBuf> {
    if self.is_path_within(&path) {
      Err(FsError::NotSupported)
    } else {
      RealFs.read_link_async(path).await
    }
  }

  fn truncate_sync(&self, path: &Path, len: u64) -> FsResult<()> {
    self.error_if_in_vfs(path)?;
    RealFs.truncate_sync(path, len)
  }
  async fn truncate_async(&self, path: PathBuf, len: u64) -> FsResult<()> {
    self.error_if_in_vfs(&path)?;
    RealFs.truncate_async(path, len).await
  }

  fn utime_sync(
    &self,
    path: &Path,
    atime_secs: i64,
    atime_nanos: u32,
    mtime_secs: i64,
    mtime_nanos: u32,
  ) -> FsResult<()> {
    self.error_if_in_vfs(path)?;
    RealFs.utime_sync(path, atime_secs, atime_nanos, mtime_secs, mtime_nanos)
  }
  async fn utime_async(
    &self,
    path: PathBuf,
    atime_secs: i64,
    atime_nanos: u32,
    mtime_secs: i64,
    mtime_nanos: u32,
  ) -> FsResult<()> {
    self.error_if_in_vfs(&path)?;
    RealFs
      .utime_async(path, atime_secs, atime_nanos, mtime_secs, mtime_nanos)
      .await
  }
}
