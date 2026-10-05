//! bgh-accounts: users, authentication, credentials, organizations, teams.
//!
//! See `docs/packages/accounts.md` for the endpoint inventory. Migrations:
//! 0100-0199.

pub mod apps;
pub mod avatars;
pub mod boot;
pub mod emails;
pub mod fine_grained;
pub mod gpg;
pub mod group_sync;
pub mod json;
pub mod keys;
pub mod ldap;
pub mod meta;
pub mod oauth;
pub mod org_two_factor;
pub mod orgs;
pub mod root;
pub mod security;
pub mod session;
pub mod social;
pub mod sso;
pub mod teams;
pub mod tokens;
pub mod totp;
pub mod twofa;
pub mod users;
pub mod util;
pub mod validate;
pub mod webauthn;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};
use bgh_core::{AppState, Registry};

pub use orgs::create_org;
pub use users::{NewAccount, create_user};

/// REST routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(meta::root))
        .route("/markdown", post(root::render))
        .route("/markdown/raw", post(root::render_raw))
        .route("/emojis", get(root::emojis))
        .route("/zen", get(root::zen))
        .route("/octocat", get(root::octocat))
        .route("/versions", get(root::versions))
        // users
        .route(
            "/user",
            get(users::get_authenticated_user).patch(users::update_authenticated_user),
        )
        .route("/user/{account_id}", get(users::get_user_by_id))
        .route("/users", get(users::list_users))
        .route("/users/{username}", get(users::get_user))
        // emails
        .route(
            "/user/emails",
            get(emails::list).post(emails::add).delete(emails::remove),
        )
        .route("/user/public_emails", get(emails::list_public))
        .route("/user/email/visibility", patch(emails::set_visibility))
        // followers
        .route("/user/followers", get(social::my_followers))
        .route("/user/following", get(social::my_following))
        .route(
            "/user/following/{username}",
            get(social::check_my_following)
                .put(social::follow)
                .delete(social::unfollow),
        )
        .route("/users/{username}/followers", get(social::followers))
        .route("/users/{username}/following", get(social::following))
        .route(
            "/users/{username}/following/{target_user}",
            get(social::check_following),
        )
        // blocks
        .route("/user/blocks", get(social::my_blocks))
        .route(
            "/user/blocks/{username}",
            get(social::check_my_block)
                .put(social::block_user)
                .delete(social::unblock_user),
        )
        // keys
        .route("/user/keys", get(keys::list_ssh).post(keys::create_ssh))
        .route(
            "/user/keys/{key_id}",
            get(keys::get_ssh).delete(keys::delete_ssh),
        )
        .route("/users/{username}/keys", get(keys::list_user_ssh))
        .route("/user/gpg_keys", get(keys::list_gpg).post(keys::create_gpg))
        .route(
            "/user/gpg_keys/{gpg_key_id}",
            get(keys::get_gpg).delete(keys::delete_gpg),
        )
        .route("/users/{username}/gpg_keys", get(keys::list_user_gpg))
        // OAuth app token API
        .route(
            "/applications/{client_id}/token",
            post(oauth::check_app_token)
                .patch(oauth::reset_app_token)
                .delete(oauth::delete_app_token),
        )
        .route(
            "/applications/{client_id}/grant",
            delete(oauth::delete_app_grant),
        )
        // GitHub Apps
        .route("/app", get(apps::rest::get_app))
        .route("/apps/{app_slug}", get(apps::rest::get_app_by_slug))
        .route("/app/installations", get(apps::rest::list_installations))
        .route(
            "/app-manifests/{code}/conversions",
            post(apps::manifest::convert),
        )
        .route(
            "/app/installations/{installation_id}",
            get(apps::rest::get_installation).delete(apps::rest::delete_installation),
        )
        .route(
            "/app/installations/{installation_id}/suspended",
            put(apps::rest::suspend).delete(apps::rest::unsuspend),
        )
        .route(
            "/app/installations/{installation_id}/access_tokens",
            post(apps::rest::create_access_token),
        )
        .route(
            "/orgs/{org}/installation",
            get(apps::rest::org_installation),
        )
        .route(
            "/users/{username}/installation",
            get(apps::rest::user_installation),
        )
        .route(
            "/repos/{owner}/{repo}/installation",
            get(apps::rest::repo_installation),
        )
        .route(
            "/installation/repositories",
            get(apps::rest::installation_repositories),
        )
        .route("/installation/token", delete(apps::rest::revoke_token))
        .route(
            "/user/installations",
            get(apps::install::user_installations),
        )
        .route(
            "/user/installations/{installation_id}/repositories",
            get(apps::install::user_installation_repos),
        )
        .route(
            "/user/installations/{installation_id}/repositories/{repository_id}",
            put(apps::install::add_user_installation_repo)
                .delete(apps::install::remove_user_installation_repo),
        )
        .route(
            "/orgs/{org}/installations",
            get(apps::install::org_installations),
        )
        // fine-grained personal access tokens (P47)
        .route(
            "/orgs/{org}/personal-access-token-requests",
            get(fine_grained::list_requests).post(fine_grained::review_requests),
        )
        .route(
            "/orgs/{org}/personal-access-token-requests/{pat_request_id}",
            post(fine_grained::review_request),
        )
        .route(
            "/orgs/{org}/personal-access-token-requests/{pat_request_id}/repositories",
            get(fine_grained::request_repositories),
        )
        .route(
            "/orgs/{org}/personal-access-tokens",
            get(fine_grained::list_grants).post(fine_grained::revoke_grants),
        )
        .route(
            "/orgs/{org}/personal-access-tokens/{pat_id}",
            post(fine_grained::revoke_grant),
        )
        .route(
            "/orgs/{org}/personal-access-tokens/{pat_id}/repositories",
            get(fine_grained::grant_repositories),
        )
        // organizations
        .route("/orgs/{org}", get(orgs::get_org).patch(orgs::update_org))
        .route("/organizations", get(orgs::list_all))
        .route("/user/orgs", get(orgs::my_orgs))
        .route("/users/{username}/orgs", get(orgs::user_orgs))
        .route("/admin/organizations", post(orgs::admin_create_org))
        .route("/orgs/{org}/members", get(orgs::list_members))
        .route(
            "/orgs/{org}/members/{username}",
            get(orgs::check_member).delete(orgs::delete_member),
        )
        .route("/orgs/{org}/public_members", get(orgs::list_public_members))
        .route(
            "/orgs/{org}/public_members/{username}",
            get(orgs::check_public_member)
                .put(orgs::publicize)
                .delete(orgs::conceal),
        )
        .route(
            "/orgs/{org}/memberships/{username}",
            get(orgs::get_membership)
                .put(orgs::set_membership)
                .delete(orgs::delete_membership),
        )
        .route("/user/memberships/orgs", get(orgs::my_memberships))
        .route(
            "/user/memberships/orgs/{org}",
            get(orgs::my_membership).patch(orgs::accept_membership),
        )
        .route(
            "/orgs/{org}/invitations",
            get(orgs::list_invitations).post(orgs::invite),
        )
        .route(
            "/orgs/{org}/invitations/{invitation_id}",
            delete(orgs::cancel_invitation),
        )
        .route(
            "/orgs/{org}/invitations/{invitation_id}/teams",
            get(orgs::invitation_teams),
        )
        .route(
            "/organizations/{org_id}/invitations/{invitation_id}/teams",
            get(orgs::invitation_teams_by_id),
        )
        .route(
            "/orgs/{org}/failed_invitations",
            get(orgs::list_failed_invitations),
        )
        .route(
            "/orgs/{org}/outside_collaborators",
            get(orgs::list_outside_collaborators),
        )
        .route(
            "/orgs/{org}/outside_collaborators/{username}",
            put(orgs::convert_to_outside_collaborator).delete(orgs::remove_outside_collaborator),
        )
        .route("/orgs/{org}/blocks", get(orgs::list_blocks))
        .route(
            "/orgs/{org}/blocks/{username}",
            get(orgs::check_block)
                .put(orgs::block_user)
                .delete(orgs::unblock_user),
        )
        // teams
        .route("/orgs/{org}/teams", get(teams::list).post(teams::create))
        .route("/user/teams", get(teams::my_teams))
        .merge(team_routes("/orgs/{org}/teams/{team_slug}"))
        .merge(team_routes("/organizations/{org_id}/team/{team_id}"))
        .merge(team_routes("/teams/{team_id}"))
}

/// Team sub-resources, mounted under each team path form (see
/// [`teams::TeamPath`]).
fn team_routes(base: &str) -> Router<AppState> {
    Router::new()
        .route(
            base,
            get(teams::get).patch(teams::update).delete(teams::delete),
        )
        .route(&format!("{base}/teams"), get(teams::children))
        .route(&format!("{base}/members"), get(teams::members))
        .route(
            &format!("{base}/memberships/{{username}}"),
            get(teams::get_membership)
                .put(teams::set_membership)
                .delete(teams::delete_membership),
        )
        .route(&format!("{base}/invitations"), get(teams::invitations))
        .route(
            &format!("{base}/team-sync/group-mappings"),
            get(group_sync::get_mappings).patch(group_sync::set_mappings),
        )
        .route(&format!("{base}/repos"), get(teams::repos))
        .route(
            &format!("{base}/repos/{{owner}}/{{repo}}"),
            get(teams::check_repo)
                .put(teams::add_repo)
                .delete(teams::remove_repo),
        )
}

/// Web-client routes (absolute paths).
pub fn web_router() -> Router<AppState> {
    Router::new()
        // `GET /api/v3/` (trailing slash, as requested by `gh`); the nested
        // API router only matches `/api/v3`.
        .route("/api/v3/", get(meta::root))
        .route("/_bgh/emoji/{file}", get(root::emoji_image))
        .route("/_bgh/boot", get(boot::get_boot))
        .route("/_bgh/auth/login", post(boot::login))
        .route("/_bgh/auth/2fa", post(boot::two_factor))
        .route("/_bgh/auth/signup", post(boot::signup))
        .route("/_bgh/auth/logout", post(boot::logout))
        // Older JSON session endpoints (kept as aliases for API clients).
        .route("/_bgh/signup", post(session::signup))
        .route(
            "/_bgh/session",
            post(session::login).delete(session::logout),
        )
        .route("/_bgh/session/two_factor", post(session::two_factor))
        .route(
            "/_bgh/sessions",
            get(session::list_sessions).delete(session::revoke_other_sessions),
        )
        .route("/_bgh/sessions/{id}", delete(session::revoke_session))
        .route("/_bgh/user/password", put(session::change_password))
        .route("/_bgh/password_reset", post(session::request_reset))
        .route(
            "/_bgh/password_reset/{token}",
            get(session::check_reset).post(session::reset_password),
        )
        .route(
            "/_bgh/user/two_factor",
            get(twofa::status).delete(twofa::disable),
        )
        .route("/_bgh/user/two_factor/totp", post(twofa::start_totp))
        .route(
            "/_bgh/user/two_factor/totp/enable",
            post(twofa::enable_totp),
        )
        .route(
            "/_bgh/user/two_factor/recovery_codes",
            post(twofa::regenerate),
        )
        // WebAuthn security keys / passkeys, sudo mode (P36)
        .route("/_bgh/user/webauthn", get(webauthn::list))
        .route(
            "/_bgh/user/webauthn/registrations",
            post(webauthn::start_registration),
        )
        .route(
            "/_bgh/user/webauthn/registrations/{id}",
            post(webauthn::finish_registration),
        )
        .route(
            "/_bgh/user/webauthn/{id}",
            patch(webauthn::rename).delete(webauthn::delete),
        )
        .route(
            "/_bgh/auth/login/passkey/challenge",
            post(webauthn::passkey_challenge),
        )
        .route("/_bgh/auth/login/passkey", post(webauthn::passkey_login))
        .route(
            "/_bgh/auth/2fa/webauthn/challenge",
            post(webauthn::two_factor_challenge),
        )
        .route("/_bgh/auth/2fa/webauthn", post(webauthn::two_factor_login))
        .route(
            "/_bgh/sudo",
            get(security::sudo_status).post(security::sudo),
        )
        .route(
            "/_bgh/sudo/webauthn/challenge",
            post(security::sudo_challenge),
        )
        .route("/_bgh/emails/verify", post(emails::verify))
        .route(
            "/_bgh/user/emails/{email}/verification",
            post(emails::resend_verification),
        )
        .route(
            "/_bgh/user/emails/{email}/primary",
            put(emails::set_primary),
        )
        .route(
            "/_bgh/tokens",
            post(tokens::create_token).get(tokens::list_tokens),
        )
        .route("/_bgh/tokens/{id}", delete(tokens::delete_token))
        .route(
            "/_bgh/fine-grained-tokens",
            get(fine_grained::list).post(fine_grained::create),
        )
        .route(
            "/_bgh/fine-grained-tokens/owners",
            get(fine_grained::owners),
        )
        .route(
            "/_bgh/fine-grained-tokens/permissions",
            get(fine_grained::permissions),
        )
        .route(
            "/_bgh/fine-grained-tokens/{id}",
            get(fine_grained::get).delete(fine_grained::delete),
        )
        .route(
            "/_bgh/orgs/{org}/pat-policy",
            get(fine_grained::get_policy).patch(fine_grained::update_policy),
        )
        .route("/_bgh/orgs", post(orgs::web_create_org))
        .route(
            "/_bgh/orgs/{org}/invitation",
            get(orgs::viewer_invitation).delete(orgs::decline_invitation),
        )
        .route("/_bgh/user/organizations", get(orgs::viewer_organizations))
        // avatars
        .route("/avatars/u/{id}", get(avatars::serve))
        .route(
            "/_bgh/user/avatar",
            put(avatars::upload_mine).delete(avatars::delete_mine),
        )
        .route(
            "/_bgh/orgs/{org}/avatar",
            put(avatars::upload_org).delete(avatars::delete_org),
        )
        // SSO
        .route("/_bgh/sso", get(sso::list))
        .route("/_bgh/sso/{id}/login", get(sso::login))
        .route("/_bgh/sso/{id}/callback", get(sso::callback))
        .route("/_bgh/user/identities", get(sso::my_identities))
        .route("/_bgh/user/identities/{id}", delete(sso::unlink_identity))
        // OAuth
        .route(
            "/login/oauth/authorize",
            get(oauth::authorize_page).post(oauth::authorize_submit),
        )
        .route("/login/oauth/access_token", post(oauth::access_token))
        .route("/login/device/code", post(oauth::device_code))
        .route(
            "/login/device",
            get(oauth::device_page).post(oauth::device_submit),
        )
        .route(
            "/_bgh/oauth/authorize",
            get(oauth::authorize_info).post(oauth::authorize_json),
        )
        .route("/_bgh/device", post(oauth::device_decide))
        .route("/_bgh/device/{user_code}", get(oauth::device_info))
        .route(
            "/_bgh/applications",
            get(oauth::list_apps).post(oauth::create_app),
        )
        .route(
            "/_bgh/applications/{id}",
            get(oauth::get_app)
                .patch(oauth::update_app)
                .delete(oauth::delete_app),
        )
        .route(
            "/_bgh/applications/{id}/client_secret",
            post(oauth::regenerate_secret),
        )
        // GitHub Apps
        .route(
            "/_bgh/apps",
            get(apps::manage::list).post(apps::manage::create),
        )
        .route(
            "/_bgh/apps/{slug}",
            get(apps::manage::get)
                .patch(apps::manage::update)
                .delete(apps::manage::delete),
        )
        .route("/settings/apps/new", post(apps::manifest::post_user))
        .route(
            "/organizations/{org}/settings/apps/new",
            post(apps::manifest::post_org),
        )
        .route(
            "/_bgh/app-manifests/{token}",
            get(apps::manifest::info).post(apps::manifest::create),
        )
        .route("/_bgh/apps/{slug}/keys", post(apps::manage::create_key))
        .route(
            "/_bgh/apps/{slug}/client_secrets",
            post(apps::manage::create_client_secret),
        )
        .route(
            "/_bgh/apps/{slug}/client_secrets/{id}",
            delete(apps::manage::delete_client_secret),
        )
        .route(
            "/_bgh/apps/{slug}/keys/{id}",
            delete(apps::manage::delete_key),
        )
        .route(
            "/_bgh/apps/{slug}/install",
            get(apps::install::install_info),
        )
        .route(
            "/_bgh/apps/{slug}/installations",
            post(apps::install::install),
        )
        .route("/_bgh/installations", get(apps::install::list_for_account))
        .route(
            "/_bgh/installations/{id}",
            get(apps::install::get_installation)
                .patch(apps::install::update_installation)
                .delete(apps::install::delete_installation),
        )
        .route(
            "/_bgh/installations/{id}/suspended",
            put(apps::install::suspend).delete(apps::install::unsuspend),
        )
        .route(
            "/_bgh/installations/{id}/accept_permissions",
            post(apps::install::accept_permissions),
        )
        .route("/_bgh/authorizations", get(oauth::list_grants))
        .route("/_bgh/authorizations/{id}", delete(oauth::delete_grant))
}

/// Background work: LDAP sync jobs and the periodic `accounts.ldap_sync`
/// service (also installs the LDAP password directory,
/// `bgh_core::auth::check_password`), and the `accounts.security` service
/// (PAT expiry reminders, legacy TOTP secret encryption). Account mail is
/// queued as the shared `mail.send` job of `bgh_core::mail`.
pub fn register(reg: &mut Registry) {
    ldap::install();
    reg.job(ldap::sync::sync_all_job);
    reg.job(ldap::sync::sync_user_job);
    reg.job(ldap::sync::sync_team_job);
    reg.service("accounts.ldap_sync", ldap::sync::service);
    reg.service("accounts.security", security::service);
}
