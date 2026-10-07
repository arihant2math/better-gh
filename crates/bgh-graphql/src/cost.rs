//! GitHub's GraphQL resource limits: the node limit and query cost.
//!
//! After validation, [`CostLimit`] walks the selected operation like GitHub
//! does (<https://docs.github.com/graphql/overview/rate-limits-and-node-limits-for-the-graphql-api>):
//!
//! * Every connection that selects `nodes` or `edges` needs `first` or
//!   `last` (`MISSING_PAGINATION_BOUNDARIES`), at most 100
//!   (`EXCESSIVE_PAGINATION`).
//! * The node count is the sum, over every connection, of the product of the
//!   page sizes on its path; more than 500,000 is `MAX_NODE_LIMIT_EXCEEDED`.
//! * The cost is the number of connection fetches (the product of the
//!   parent page sizes, summed) divided by 100 and rounded, at least 1. The
//!   root middleware already counted 1 point against the `graphql` budget;
//!   the rest is charged here, and `rateLimit { cost nodeCount }` reports it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_graphql::extensions::{
    Extension, ExtensionContext, ExtensionFactory, NextParseQuery, NextPrepareRequest,
    NextValidation,
};
use async_graphql::parser::types::OperationType;
use async_graphql::parser::types::{
    DocumentOperations, ExecutableDocument, OperationDefinition, Selection, SelectionSet,
};
use async_graphql::registry::{MetaTypeName, Registry};
use async_graphql::{
    ErrorExtensionValues, Name, PathSegment, Pos, Request, ServerError, ServerResult,
    ValidationResult, Value, Variables,
};
use bgh_core::ratelimit::{self, Quota, Resource};

use crate::conn::MAX_PAGE;
use crate::ctx::Gql;

/// Most nodes one query may request.
pub const MAX_NODES: i64 = 500_000;

/// Selections the walk visits at most (fragment expansion can blow up a
/// small document).
const MAX_VISITS: usize = 100_000;

/// The static cost of an operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cost {
    /// Nodes the query can return at most.
    pub nodes: i64,
    /// Connection fetches needed to resolve it.
    pub requests: i64,
}

impl Cost {
    /// Rate-limit points: `requests / 100`, rounded, at least 1.
    pub fn points(&self) -> i64 {
        ((self.requests + 50) / 100).max(1)
    }
}

/// Request data: the cost computed for this request and the quota after
/// charging it (when the rate limiter is available).
#[derive(Default)]
pub struct CostCell(OnceLock<(Cost, Option<Quota>)>);

impl CostCell {
    pub fn cost(&self) -> Option<Cost> {
        self.0.get().map(|(c, _)| *c)
    }

    pub fn quota(&self) -> Option<Quota> {
        self.0.get().and_then(|(_, q)| *q)
    }
}

/// Schema extension enforcing the limits (see the module docs).
pub struct CostLimit;

impl ExtensionFactory for CostLimit {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(CostExt::default())
    }
}

#[derive(Default)]
struct CostExt {
    operation: Mutex<Option<String>>,
    document: Mutex<Option<(ExecutableDocument, Variables)>>,
}

#[async_graphql::async_trait::async_trait]
impl Extension for CostExt {
    async fn prepare_request(
        &self,
        ctx: &ExtensionContext<'_>,
        request: Request,
        next: NextPrepareRequest<'_>,
    ) -> ServerResult<Request> {
        *self.operation.lock().unwrap() = request.operation_name.clone();
        next.run(ctx, request).await
    }

    async fn parse_query(
        &self,
        ctx: &ExtensionContext<'_>,
        query: &str,
        variables: &Variables,
        next: NextParseQuery<'_>,
    ) -> ServerResult<ExecutableDocument> {
        let doc = next.run(ctx, query, variables).await?;
        *self.document.lock().unwrap() = Some((doc.clone(), variables.clone()));
        Ok(doc)
    }

    async fn validation(
        &self,
        ctx: &ExtensionContext<'_>,
        next: NextValidation<'_>,
    ) -> Result<ValidationResult, Vec<ServerError>> {
        let result = next.run(ctx).await?;
        let Some((doc, vars)) = self.document.lock().unwrap().take() else {
            return Ok(result);
        };
        let op = self.operation.lock().unwrap().take();
        let cost =
            analyze(&ctx.schema_env.registry, &doc, op.as_deref(), &vars).map_err(|e| vec![e])?;
        let quota = match ctx.data_opt::<Gql>() {
            Some(g) => charge(g, cost).await?,
            None => None,
        };
        if let Some(cell) = ctx.data_opt::<Arc<CostCell>>() {
            let _ = cell.0.set((cost, quota));
        }
        Ok(result)
    }
}

/// Charge the points beyond the one the root middleware counted; reject
/// with `RATE_LIMITED` when enforcement is on and the budget is spent.
async fn charge(g: &Gql, cost: Cost) -> Result<Option<Quota>, Vec<ServerError>> {
    let Ok(settings) = bgh_core::settings::load(&g.state).await else {
        return Ok(None);
    };
    let extra = cost.points() - 1;
    let q = if extra > 0 {
        ratelimit::charge(
            &g.state,
            &settings.rate_limits,
            Resource::Graphql,
            g.auth.as_ref(),
            &g.client_ip,
            extra,
        )
        .await
    } else {
        ratelimit::quota(
            &g.state,
            &settings.rate_limits,
            Resource::Graphql,
            g.auth.as_ref(),
            &g.client_ip,
            false,
        )
        .await
    };
    let Ok(q) = q else {
        tracing::warn!("rate limiter unavailable; not charging GraphQL cost");
        return Ok(None);
    };
    if settings.rate_limits.enabled && q.exceeded() {
        let message = match &g.auth {
            Some(a) => format!("API rate limit exceeded for user ID {}.", a.user.id),
            None => format!("API rate limit exceeded for {}.", g.client_ip),
        };
        return Err(vec![error("RATE_LIMITED", message, None, Vec::new())]);
    }
    Ok(Some(q))
}

fn error(ty: &str, message: String, pos: Option<Pos>, path: Vec<String>) -> ServerError {
    let mut e = ServerError::new(message, pos);
    e.path = path.into_iter().map(PathSegment::Field).collect();
    let mut ext = ErrorExtensionValues::default();
    ext.set("type", ty);
    e.extensions = Some(ext);
    e
}

/// `1234567` -> `1,234,567` (GitHub's number format in limit errors).
fn thousands(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The cost of the operation `op` (or the only one) of a validated
/// document.
pub fn analyze(
    registry: &Registry,
    doc: &ExecutableDocument,
    op: Option<&str>,
    vars: &Variables,
) -> Result<Cost, ServerError> {
    let operation = match (&doc.operations, op) {
        (DocumentOperations::Single(o), _) => &o.node,
        (DocumentOperations::Multiple(m), Some(name)) => match m.get(name) {
            Some(o) => &o.node,
            None => return Ok(Cost::default()),
        },
        (DocumentOperations::Multiple(m), None) => match m.values().next() {
            Some(o) if m.len() == 1 => &o.node,
            _ => return Ok(Cost::default()),
        },
    };
    let root = match operation.ty {
        OperationType::Query => registry.query_type.as_str(),
        OperationType::Mutation => match registry.mutation_type.as_deref() {
            Some(t) => t,
            None => return Ok(Cost::default()),
        },
        OperationType::Subscription => {
            return Ok(Cost::default());
        }
    };
    let mut w = Walker {
        registry,
        doc,
        vars: variables(operation, vars),
        cost: Cost::default(),
        visits: 0,
        path: Vec::new(),
    };
    w.walk(&operation.selection_set.node, root, 1)?;
    if w.cost.nodes > MAX_NODES {
        return Err(error(
            "MAX_NODE_LIMIT_EXCEEDED",
            format!(
                "This query requests up to {} possible nodes which exceeds the maximum limit of {}.",
                thousands(w.cost.nodes),
                thousands(MAX_NODES),
            ),
            None,
            Vec::new(),
        ));
    }
    Ok(w.cost)
}

/// Variable values: the request's, else the operation's defaults.
fn variables(op: &OperationDefinition, vars: &Variables) -> HashMap<Name, Value> {
    let mut out: HashMap<Name, Value> = op
        .variable_definitions
        .iter()
        .filter_map(|d| {
            let v = d.node.default_value.as_ref()?;
            Some((d.node.name.node.clone(), v.node.clone()))
        })
        .collect();
    out.extend(vars.iter().map(|(k, v)| (k.clone(), v.clone())));
    out
}

struct Walker<'a> {
    registry: &'a Registry,
    doc: &'a ExecutableDocument,
    vars: HashMap<Name, Value>,
    cost: Cost,
    visits: usize,
    path: Vec<String>,
}

impl Walker<'_> {
    fn walk(&mut self, set: &SelectionSet, ty: &str, mult: i64) -> Result<(), ServerError> {
        for item in &set.items {
            self.visits += 1;
            if self.visits > MAX_VISITS {
                return Err(error(
                    "MAX_NODE_LIMIT_EXCEEDED",
                    "This query has too many selections to evaluate.".into(),
                    Some(item.pos),
                    Vec::new(),
                ));
            }
            match &item.node {
                Selection::Field(f) => {
                    let field = &f.node;
                    let Some(def) = self
                        .registry
                        .concrete_type_by_name(ty)
                        .and_then(|t| t.field_by_name(&field.name.node))
                    else {
                        continue;
                    };
                    let child = MetaTypeName::concrete_typename(&def.ty);
                    self.path.push(field.response_key().node.to_string());
                    let mut child_mult = mult;
                    if child.ends_with("Connection") && def.args.contains_key("first") {
                        let name = field.name.node.as_str();
                        let mut page = None;
                        for arg in ["first", "last"] {
                            // The argument with variables substituted.
                            let value = field.get_argument(arg).and_then(|v| {
                                v.node
                                    .clone()
                                    .into_const_with(|n| self.vars.get(&n).cloned().ok_or(()))
                                    .ok()
                            });
                            let Some(Value::Number(n)) = value else {
                                continue;
                            };
                            let Some(n) = n.as_i64() else { continue };
                            if n > MAX_PAGE {
                                return Err(error(
                                    "EXCESSIVE_PAGINATION",
                                    format!(
                                        "Requesting {n} records on the `{name}` connection \
                                         exceeds the `{arg}` limit of {MAX_PAGE} records."
                                    ),
                                    Some(f.pos),
                                    self.path.clone(),
                                ));
                            }
                            page = Some(page.unwrap_or(0).max(n.max(0)));
                        }
                        let page = match page {
                            Some(n) => n,
                            None if self.selects_items(&field.selection_set.node, 0) => {
                                return Err(error(
                                    "MISSING_PAGINATION_BOUNDARIES",
                                    format!(
                                        "You must provide a `first` or `last` value to \
                                         properly paginate the `{name}` connection."
                                    ),
                                    Some(f.pos),
                                    self.path.clone(),
                                ));
                            }
                            // `totalCount` / `pageInfo` only.
                            None => 0,
                        };
                        self.cost.requests = self.cost.requests.saturating_add(mult);
                        child_mult = mult.saturating_mul(page);
                        self.cost.nodes = self.cost.nodes.saturating_add(child_mult);
                    }
                    let r = self.walk(&field.selection_set.node, child, child_mult);
                    self.path.pop();
                    r?;
                }
                Selection::InlineFragment(f) => {
                    let ty = f
                        .node
                        .type_condition
                        .as_ref()
                        .map_or(ty, |c| c.node.on.node.as_str());
                    self.walk(&f.node.selection_set.node, ty, mult)?;
                }
                Selection::FragmentSpread(s) => {
                    let Some(def) = self.doc.fragments.get(&s.node.fragment_name.node) else {
                        continue;
                    };
                    let on = def.node.type_condition.node.on.node.as_str();
                    self.walk(&def.node.selection_set.node, on, mult)?;
                }
            }
        }
        Ok(())
    }

    /// Whether a connection's selection reads `nodes` or `edges`.
    fn selects_items(&self, set: &SelectionSet, depth: usize) -> bool {
        depth < 8
            && set.items.iter().any(|item| match &item.node {
                Selection::Field(f) => matches!(f.node.name.node.as_str(), "nodes" | "edges"),
                Selection::InlineFragment(f) => {
                    self.selects_items(&f.node.selection_set.node, depth + 1)
                }
                Selection::FragmentSpread(s) => self
                    .doc
                    .fragments
                    .get(&s.node.fragment_name.node)
                    .is_some_and(|d| self.selects_items(&d.node.selection_set.node, depth + 1)),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_and_format() {
        assert_eq!(Cost::default().points(), 1);
        assert_eq!(
            Cost {
                nodes: 0,
                requests: 149
            }
            .points(),
            1
        );
        assert_eq!(
            Cost {
                nodes: 0,
                requests: 150
            }
            .points(),
            2
        );
        assert_eq!(
            Cost {
                nodes: 0,
                requests: 20_100
            }
            .points(),
            201
        );
        assert_eq!(thousands(500_000), "500,000");
        assert_eq!(thousands(1_000_000), "1,000,000");
        assert_eq!(thousands(999), "999");
    }
}
