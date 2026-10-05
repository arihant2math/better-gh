//! Built-in bot accounts.
//!
//! `github-actions[bot]` authors everything done with an Actions job token
//! (`GITHUB_TOKEN`): comments, labels, pushes. It is a `Bot` user with
//! GitHub's id (41898282, so scripts comparing ids keep working), created
//! on first use by [`ensure_actions_bot`]; nobody can sign in as it (no
//! password, and `[`/`]` are not allowed in sign-up logins).

/// Login of the Actions bot.
pub const ACTIONS_BOT_LOGIN: &str = "github-actions[bot]";
/// Id of the Actions bot (GitHub's).
pub const ACTIONS_BOT_ID: i64 = 41898282;

/// Whether `user_id` is the Actions bot: events it caused came from a job
/// token (see `bgh-actions` trigger loop guard).
pub fn is_actions_bot(user_id: Option<i64>) -> bool {
    user_id == Some(ACTIONS_BOT_ID)
}

/// Create the Actions bot if missing; returns its id.
pub async fn ensure_actions_bot(conn: &mut sqlx::PgConnection) -> Result<i64, sqlx::Error> {
    sqlx::query(
        "INSERT INTO users (id, login, type, name)
         OVERRIDING SYSTEM VALUE VALUES ($1, $2, 'Bot', 'github-actions')
         ON CONFLICT DO NOTHING",
    )
    .bind(ACTIONS_BOT_ID)
    .bind(ACTIONS_BOT_LOGIN)
    .execute(&mut *conn)
    .await?;
    sqlx::query_scalar("SELECT id FROM users WHERE lower(login) = lower($1)")
        .bind(ACTIONS_BOT_LOGIN)
        .fetch_one(&mut *conn)
        .await
}
