//! Shared foundation for all Better GitHub crates.
//!
//! See `docs/BACKEND_PATTERNS.md` for how the pieces fit together.

pub mod audit;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod events;
pub mod extract;
pub mod jobs;
pub mod labels;
pub mod mail;
pub mod markdown;
pub mod models;
pub mod node_id;
pub mod outbox;
pub mod pagination;
pub mod perms;
pub mod ratelimit;
pub mod registry;
pub mod settings;
pub mod state;
pub mod sync;
pub mod time;
pub mod two_factor;
pub mod urls;
pub mod views;

#[cfg(feature = "testing")]
pub mod testing;

pub use config::Config;
pub use error::{ApiError, ApiResult, FieldError};
pub use registry::Registry;
pub use state::AppState;

/// Common imports for handler modules: `use bgh_core::prelude::*;`
pub mod prelude {
    pub use crate::auth::{AuthContext, MaybeUser, RequireSiteAdmin, RequireUser};
    pub use crate::db::Tx;
    pub use crate::error::{ApiError, ApiResult, FieldError};
    pub use crate::events::Event;
    pub use crate::extract::{Json, Path, Query};
    pub use crate::models::{api, db};
    pub use crate::pagination::{Page, Pagination};
    pub use crate::perms::{Permission, RepoAccess};
    pub use crate::registry::Registry;
    pub use crate::state::AppState;
    pub use crate::sync::SyncAction;
    pub use crate::sync::shapes::Model as SyncModel;
    pub use crate::time::Timestamp;
}
