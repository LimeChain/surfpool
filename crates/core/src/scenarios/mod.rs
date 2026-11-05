pub mod keep_alive;
pub mod protocols;
pub mod registry;

pub use registry::TemplateRegistry;
pub(crate) use registry::{account_data_values, template_registry};

/// How to write an override template collection at runtime; the MCP server serves it as a
/// prompt and a resource.
pub const AUTHORING_GUIDE: &str = include_str!("AUTHORING.md");
