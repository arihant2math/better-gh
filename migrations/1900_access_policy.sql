-- P7 access policy: organizations can stop members from creating internal
-- repositories (`members_can_create_internal_repositories`, PATCH /orgs).
ALTER TABLE org_settings
    ADD COLUMN members_can_create_internal_repositories BOOLEAN NOT NULL DEFAULT true;
