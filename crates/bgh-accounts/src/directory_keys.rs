//! SSH and GPG keys imported from a directory (LDAP attributes, SAML
//! attributes): rows flagged `ldap_synced` / `saml_synced` are replaced on
//! every sync; keys users added themselves are never touched.

use bgh_core::prelude::*;
use serde_json::json;

use crate::gpg;
use crate::keys::parse_ssh_key;

/// Where directory keys come from (the flag column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Ldap,
    Saml,
}

impl Source {
    fn column(self) -> &'static str {
        match self {
            Source::Ldap => "ldap_synced",
            Source::Saml => "saml_synced",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Source::Ldap => "LDAP",
            Source::Saml => "SAML",
        }
    }
}

/// Replace the directory SSH keys of `user` with `keys`.
pub async fn sync_ssh_keys(
    tx: &mut Tx,
    user: &db::User,
    keys: &[String],
    source: Source,
) -> ApiResult<()> {
    let col = source.column();
    let parsed: Vec<_> = keys.iter().filter_map(|k| parse_ssh_key(k)).collect();
    let fingerprints: Vec<String> = parsed.iter().map(|k| k.fingerprint.clone()).collect();
    sqlx::query(&format!(
        "DELETE FROM ssh_keys WHERE user_id = $1 AND {col} AND NOT (fingerprint = ANY($2))"
    ))
    .bind(user.id)
    .bind(&fingerprints)
    .execute(&mut **tx)
    .await?;
    for key in parsed {
        sqlx::query(&format!(
            "INSERT INTO ssh_keys (user_id, title, key, fingerprint, {col})
             SELECT $1, $2, $3, $4, true
              WHERE NOT EXISTS (SELECT 1 FROM deploy_keys WHERE fingerprint = $4)
             ON CONFLICT (fingerprint) DO NOTHING"
        ))
        .bind(user.id)
        .bind(key.comment.as_deref().unwrap_or(source.title()))
        .bind(&key.normalized)
        .bind(&key.fingerprint)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Replace the directory GPG keys of `user` with `keys` (armored).
pub async fn sync_gpg_keys(
    tx: &mut Tx,
    user: &db::User,
    keys: &[String],
    source: Source,
) -> ApiResult<()> {
    let col = source.column();
    let parsed: Vec<_> = keys
        .iter()
        .filter_map(|k| gpg::parse_armored(k).ok().map(|p| (k, p)))
        .collect();
    let ids: Vec<String> = parsed.iter().map(|(_, p)| p.key_id.clone()).collect();
    sqlx::query(&format!(
        "DELETE FROM gpg_keys WHERE user_id = $1 AND {col} AND primary_key_id IS NULL
           AND NOT (key_id = ANY($2))"
    ))
    .bind(user.id)
    .bind(&ids)
    .execute(&mut **tx)
    .await?;
    for (armored, key) in parsed {
        let primary_id: Option<i64> = sqlx::query_scalar(&format!(
            "INSERT INTO gpg_keys (user_id, name, key_id, public_key, raw_key, emails, can_sign,
                                   can_encrypt_comms, can_encrypt_storage, can_certify,
                                   expires_at, {col})
             SELECT $1, $11, $2, $3, $4, $5, $6, $7, $8, $9, $10, true
              WHERE NOT EXISTS (SELECT 1 FROM gpg_keys WHERE user_id = $1 AND key_id = $2)
             RETURNING id"
        ))
        .bind(user.id)
        .bind(&key.key_id)
        .bind(&key.public_key)
        .bind(armored.trim())
        .bind(json!(
            key.emails
                .iter()
                .map(|e| json!({ "email": e, "verified": true }))
                .collect::<Vec<_>>()
        ))
        .bind(key.can_sign)
        .bind(key.can_encrypt_comms)
        .bind(key.can_encrypt_storage)
        .bind(key.can_certify)
        .bind(key.expires_at)
        .bind(source.title())
        .fetch_optional(&mut **tx)
        .await?;
        let Some(primary_id) = primary_id else {
            continue;
        };
        for sub in &key.subkeys {
            sqlx::query(&format!(
                "INSERT INTO gpg_keys (user_id, key_id, primary_key_id, public_key, can_sign,
                                       can_encrypt_comms, can_encrypt_storage, can_certify,
                                       expires_at, {col})
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, true)"
            ))
            .bind(user.id)
            .bind(&sub.key_id)
            .bind(primary_id)
            .bind(&sub.public_key)
            .bind(sub.can_sign)
            .bind(sub.can_encrypt_comms)
            .bind(sub.can_encrypt_storage)
            .bind(sub.can_certify)
            .bind(sub.expires_at)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}
