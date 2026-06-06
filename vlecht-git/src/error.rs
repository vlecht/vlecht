#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git repository not found at {0}")]
    NotFound(std::path::PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("gix error: {0}")]
    Gix(String),
    #[error("pack protocol error: {0}")]
    Protocol(String),
    #[error("delta application error: {0}")]
    Delta(String),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

impl From<flate2::CompressError> for GitError {
    fn from(e: flate2::CompressError) -> Self {
        GitError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
    }
}

macro_rules! from_gix {
    ($($t:ty),* $(,)?) => {
        $(
            impl From<$t> for GitError {
                fn from(e: $t) -> Self {
                    GitError::Gix(e.to_string())
                }
            }
        )*
    };
}

from_gix!(
    gix::open::Error,
    gix::init::Error,
    gix::reference::iter::Error,
    gix::reference::find::existing::Error,
    gix::reference::find::Error,
    gix::reference::edit::Error,
    gix::validate::reference::name::Error,
    gix::object::try_into::Error,
    gix::object::commit::Error,
    gix::objs::decode::Error,
    gix::objs::find::existing::Error,
    gix::repository::diff_tree_to_tree::Error,
    gix::repository::merge_base::Error,
    gix::revision::walk::Error,
    gix::revision::spec::parse::single::Error,
    gix::revision::walk::iter::Error,
);
