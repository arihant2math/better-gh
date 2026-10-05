-- P8: fine-grained permission map of a token (`{"contents": "read", ...}`,
-- see bgh_core::token_permissions). Set for Actions job tokens (from the
-- workflow's `permissions:`) and reusable by installation tokens. NULL for
-- classic PAT / OAuth tokens, which are limited by their scopes only.
ALTER TABLE access_tokens ADD COLUMN permissions JSONB;
