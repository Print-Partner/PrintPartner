mod http;
pub(crate) mod wire;

pub use http::{CatalogHttpConfig, admit_catalog_routes};
#[cfg(test)]
pub(crate) use http::{catalog_error, naming_error};
