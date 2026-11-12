mod http;
pub(crate) mod wire;

#[cfg(feature = "desktop_rebuild")]
pub use http::upload::{
    SourceUploads, UploadFailure, UploadPart, UploadParts, admit_upload_routes,
};
pub use http::{CatalogHttpConfig, admit_catalog_routes};
#[cfg(test)]
pub(crate) use http::{catalog_error, naming_error};
