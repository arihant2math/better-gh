//! The `Mutation` root. Writes call the owning domain crates so business
//! rules, sync records and events stay in one place.

use async_graphql::{Context, Object};

use crate::ctx::{GResult, err};

/// Marker inserted for GET requests: mutations are refused.
pub struct ReadOnly;

pub fn guard<'a>(ctx: &Context<'a>) -> GResult<&'a bgh_core::auth::AuthContext> {
    if ctx.data_opt::<ReadOnly>().is_some() {
        return Err(err("FORBIDDEN", "Mutations are not allowed over GET."));
    }
    crate::ctx::gql(ctx).require_auth()
}

#[derive(Default)]
pub struct Mutation;

#[Object]
impl Mutation {
    /// Placeholder until the domain mutations are wired.
    pub async fn noop(&self, ctx: &Context<'_>) -> GResult<bool> {
        guard(ctx)?;
        Ok(true)
    }
}
