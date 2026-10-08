/**
 * `/organizations/:org/settings/personal-access-tokens`: the org's personal
 * access token policy, pending fine-grained token requests (approve / deny)
 * and the fine-grained tokens with access to the org (revoke).
 */
import { useEffect, useState, type ReactNode } from "react";
import { mutate, useResource } from "../../api/cache";
import { isAccessError } from "../../api/errors";
import {
  getPatPolicy,
  patGrantRepositories,
  patGrantsPath,
  patRequestRepositories,
  patRequestsPath,
  reviewPatRequest,
  revokePatGrant,
  updatePatPolicy,
  type OrgPatGrant,
  type PatPolicy,
} from "../../api/fineGrainedTokens";
import styles from "../../components/admin/admin.module.css";
import { formatDateTime } from "../../components/admin/format";
import {
  PageHeader,
  Panel,
  StatusPill,
  Switch,
  errorMessage,
  useConfirm,
} from "../../components/admin/kit";
import { usePagedList } from "../../api/usePagedList";
import { useParams } from "../../router";
import { Avatar } from "../../ui/Badge";
import { Button } from "../../ui/Button";
import { EmptyState, Skeleton } from "../../ui/EmptyState";
import {
  AlertIcon,
  CheckIcon,
  KeyIcon,
  LockIcon,
  RepoIcon,
  XIcon,
} from "../../ui/icons";
import { Field, Input } from "../../ui/Input";
import { RelativeTime } from "../../ui/RelativeTime";
import { toast } from "../../ui/Toast";
import {
  MAX_TOKEN_DAYS,
  selectionText,
  summarizePermissions,
} from "../settings/developer/fineGrained";
import { OwnerRequired } from "./common";
import local from "./OrgSettings.module.css";

const policyKey = (org: string) => `org:pat-policy:${org}`;

export default function OrgPatPage() {
  const { org = "" } = useParams<{ org: string }>();
  const policy = useResource(policyKey(org), () => getPatPolicy(org));
  if (isAccessError(policy.error)) {
    return (
      <div className={styles.page}>
        <PageHeader title="Personal access tokens" />
        <OwnerRequired org={org} what="manage personal access tokens" />
      </div>
    );
  }
  return (
    <div className={styles.page}>
      <PageHeader
        title="Personal access tokens"
        description={`Control how personal access tokens can access ${org}, and review fine-grained tokens that request access.`}
      />
      <div className={styles.stack}>
        {policy.data ? (
          <PolicyForm
            key={JSON.stringify(policy.data)}
            org={org}
            policy={policy.data}
          />
        ) : policy.error ? (
          <Panel title="Policy">
            <p className={styles.muted} role="alert">
              {errorMessage(policy.error)}
            </p>
          </Panel>
        ) : (
          <Panel title="Policy">
            <div className={styles.stack}>
              {[0, 1, 2].map((i) => (
                <Skeleton key={i} height={28} />
              ))}
            </div>
          </Panel>
        )}
        <TokenLists org={org} />
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ policy

/** `''` = no limit; otherwise whole days 1–366. */
function parseLifetime(v: string): { value: number | null; error?: string } {
  if (!v.trim()) return { value: null };
  const n = Number(v);
  if (!Number.isInteger(n) || n < 1 || n > MAX_TOKEN_DAYS)
    return {
      value: null,
      error: `Enter a number of days between 1 and ${MAX_TOKEN_DAYS}, or leave empty for no limit`,
    };
  return { value: n };
}

function PolicyForm({ org, policy }: { org: string; policy: PatPolicy }) {
  const [form, setForm] = useState(() => ({
    fine_grained_allowed: policy.fine_grained_allowed,
    fine_grained_require_approval: policy.fine_grained_require_approval,
    fine_grained_max: policy.fine_grained_max_lifetime_days?.toString() ?? "",
    classic_allowed: policy.classic_allowed,
    classic_max: policy.classic_max_lifetime_days?.toString() ?? "",
  }));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fgMax = parseLifetime(form.fine_grained_max);
  const clMax = parseLifetime(form.classic_max);
  const patch: Partial<PatPolicy> = {};
  if (form.fine_grained_allowed !== policy.fine_grained_allowed)
    patch.fine_grained_allowed = form.fine_grained_allowed;
  if (
    form.fine_grained_require_approval !== policy.fine_grained_require_approval
  )
    patch.fine_grained_require_approval = form.fine_grained_require_approval;
  if (!fgMax.error && fgMax.value !== policy.fine_grained_max_lifetime_days)
    patch.fine_grained_max_lifetime_days = fgMax.value;
  if (form.classic_allowed !== policy.classic_allowed)
    patch.classic_allowed = form.classic_allowed;
  if (!clMax.error && clMax.value !== policy.classic_max_lifetime_days)
    patch.classic_max_lifetime_days = clMax.value;
  const dirty = Object.keys(patch).length > 0;
  const invalid = !!(fgMax.error || clMax.error);
  const set = (p: Partial<typeof form>) => {
    setForm((f) => ({ ...f, ...p }));
    setError(null);
  };

  const save = async () => {
    if (!dirty || invalid || saving) return;
    setSaving(true);
    setError(null);
    try {
      const next = await updatePatPolicy(org, patch);
      mutate<PatPolicy>(policyKey(org), () => next);
      toast({ kind: "success", title: "Personal access token policy saved" });
    } catch (e) {
      setError(errorMessage(e));
      setSaving(false);
    }
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
      aria-label="Personal access token policy"
    >
      <Panel
        title="Policy"
        actions={
          <Button
            type="submit"
            size="sm"
            variant="primary"
            disabled={!dirty || invalid}
            loading={saving}
          >
            Save policy
          </Button>
        }
      >
        <div className={styles.form}>
          <div>
            <div className={local.sectionLabel}>
              Fine-grained personal access tokens
            </div>
            <p className={local.sectionHint}>
              Tokens scoped to {org}, a set of its repositories and specific
              permissions.
            </p>
          </div>
          <Switch
            label="Allow access via fine-grained personal access tokens"
            description={`Members can create fine-grained tokens with ${org} as the resource owner.`}
            checked={form.fine_grained_allowed}
            onChange={(v) => set({ fine_grained_allowed: v })}
          />
          {form.fine_grained_allowed && (
            <div className={local.nested}>
              <Switch
                label="Require administrator approval"
                description="New fine-grained tokens stay pending until an owner approves them here."
                checked={form.fine_grained_require_approval}
                onChange={(v) => set({ fine_grained_require_approval: v })}
              />
              <Field
                label="Maximum lifetime (days)"
                htmlFor="pat-fg-max"
                hint="Leave empty for no limit beyond the 366-day maximum."
                error={fgMax.error}
              >
                <div style={{ maxWidth: 160 }}>
                  <Input
                    id="pat-fg-max"
                    type="number"
                    min={1}
                    max={MAX_TOKEN_DAYS}
                    value={form.fine_grained_max}
                    invalid={!!fgMax.error}
                    onChange={(e) => set({ fine_grained_max: e.target.value })}
                  />
                </div>
              </Field>
            </div>
          )}
          <div>
            <div className={local.sectionLabel}>
              Personal access tokens (classic)
            </div>
            <p className={local.sectionHint}>
              Classic tokens carry OAuth scopes and reach every organization
              their owner belongs to.
            </p>
          </div>
          <Switch
            label="Allow access via personal access tokens (classic)"
            description={`When off, classic tokens cannot access ${org}’s resources.`}
            checked={form.classic_allowed}
            onChange={(v) => set({ classic_allowed: v })}
          />
          {form.classic_allowed && (
            <div className={local.nested}>
              <Field
                label="Classic token maximum lifetime (days)"
                htmlFor="pat-classic-max"
                hint="Classic tokens with a longer (or no) expiration cannot access the organization."
                error={clMax.error}
              >
                <div style={{ maxWidth: 160 }}>
                  <Input
                    id="pat-classic-max"
                    type="number"
                    min={1}
                    max={MAX_TOKEN_DAYS}
                    value={form.classic_max}
                    invalid={!!clMax.error}
                    onChange={(e) => set({ classic_max: e.target.value })}
                  />
                </div>
              </Field>
            </div>
          )}
          {error && (
            <div className={styles.formError} role="alert">
              <AlertIcon size={14} /> {error}
            </div>
          )}
        </div>
      </Panel>
    </form>
  );
}

// ------------------------------------------------------------------ requests and grants

function TokenLists({ org }: { org: string }) {
  const requests = usePagedList<OrgPatGrant>(patRequestsPath(org));
  const grants = usePagedList<OrgPatGrant>(patGrantsPath(org));
  const { reload: reloadRequests } = requests;
  const { reload: reloadGrants } = grants;
  // Requests arrive from other members at any time: always revalidate on open
  // (the cached page renders meanwhile).
  useEffect(() => {
    void reloadRequests();
    void reloadGrants();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once per org
  }, [org]);
  const confirm = useConfirm();
  const [busy, setBusy] = useState<number | null>(null);

  const approve = async (r: OrgPatGrant) => {
    setBusy(r.id);
    try {
      await reviewPatRequest(org, r.id, "approve");
      requests.update((items) => items.filter((x) => x.id !== r.id));
      void grants.reload();
      toast({ kind: "success", title: `Approved ${r.token_name}` });
    } catch (e) {
      toast({
        kind: "error",
        title: "Could not approve the request",
        description: errorMessage(e),
      });
    } finally {
      setBusy(null);
    }
  };
  const deny = (r: OrgPatGrant) =>
    confirm({
      title: `Deny access for ${r.token_name}?`,
      body: `${r.owner.login}’s token will not be able to access ${org}’s resources.`,
      confirmLabel: "Deny request",
      danger: true,
      reason: {
        label: "Reason (optional)",
        placeholder: "Shared with the requester",
      },
      onConfirm: async (reason) => {
        await reviewPatRequest(org, r.id, "deny", reason || undefined);
        requests.update((items) => items.filter((x) => x.id !== r.id));
        toast({ kind: "success", title: `Denied ${r.token_name}` });
      },
    });
  const revoke = (g: OrgPatGrant) =>
    confirm({
      title: `Revoke ${g.token_name}?`,
      body: `${g.owner.login}’s token immediately loses access to ${org}’s resources. You cannot undo this.`,
      confirmLabel: "Revoke token",
      danger: true,
      onConfirm: async () => {
        await revokePatGrant(org, g.id);
        grants.update((items) => items.filter((x) => x.id !== g.id));
        toast({ kind: "success", title: `Revoked ${g.token_name}` });
      },
    });

  return (
    <>
      <Panel
        title={`Pending requests${requests.done ? ` (${requests.items.length})` : ""}`}
        padded={false}
      >
        <GrantList
          org={org}
          kind="request"
          list={requests}
          empty="No fine-grained tokens are waiting for approval."
          actions={(r) => (
            <>
              <Button
                size="sm"
                variant="success"
                leadingIcon={CheckIcon}
                loading={busy === r.id}
                onClick={() => void approve(r)}
                aria-label={`Approve ${r.token_name}`}
              >
                Approve
              </Button>
              <Button
                size="sm"
                variant="danger"
                leadingIcon={XIcon}
                disabled={busy === r.id}
                onClick={() => deny(r)}
                aria-label={`Deny ${r.token_name}`}
              >
                Deny
              </Button>
            </>
          )}
        />
      </Panel>
      <Panel
        title={`Active tokens${grants.done ? ` (${grants.items.length})` : ""}`}
        padded={false}
      >
        <GrantList
          org={org}
          kind="grant"
          list={grants}
          empty={`No fine-grained tokens have access to ${org}.`}
          actions={(g) => (
            <Button
              size="sm"
              variant="danger"
              onClick={() => revoke(g)}
              aria-label={`Revoke ${g.token_name}`}
            >
              Revoke
            </Button>
          )}
        />
      </Panel>
      {confirm.dialog}
    </>
  );
}

function GrantList({
  org,
  kind,
  list,
  empty,
  actions,
}: {
  org: string;
  kind: "request" | "grant";
  list: {
    items: OrgPatGrant[];
    loading: boolean;
    error: unknown;
    next: string | null;
    loadMore: () => unknown;
  };
  empty: string;
  actions: (g: OrgPatGrant) => ReactNode;
}) {
  if (list.error && !list.items.length) {
    return (
      <EmptyState icon={AlertIcon} title="Could not load tokens">
        {errorMessage(list.error)}
      </EmptyState>
    );
  }
  if (list.loading && !list.items.length) {
    return (
      <div className={styles.panelBody}>
        <Skeleton width="60%" />
      </div>
    );
  }
  if (!list.items.length) {
    return (
      <div className={styles.panelBody}>
        <p className={styles.muted} style={{ margin: 0 }}>
          {empty}
        </p>
      </div>
    );
  }
  return (
    <>
      <ul
        className={styles.list}
        aria-label={kind === "request" ? "Pending requests" : "Active tokens"}
      >
        {list.items.map((g) => (
          <GrantRow
            key={g.id}
            org={org}
            kind={kind}
            g={g}
            actions={actions(g)}
          />
        ))}
      </ul>
      {list.next && (
        <div className={styles.panelBody}>
          <Button
            size="sm"
            loading={list.loading}
            onClick={() => void list.loadMore()}
          >
            Load more
          </Button>
        </div>
      )}
    </>
  );
}

function GrantRow({
  org,
  kind,
  g,
  actions,
}: {
  org: string;
  kind: "request" | "grant";
  g: OrgPatGrant;
  actions: ReactNode;
}) {
  const [showRepos, setShowRepos] = useState(false);
  const repos = useResource(
    showRepos ? `org:pat-repos:${org}/${kind}/${g.id}` : null,
    () =>
      kind === "request"
        ? patRequestRepositories(org, g.id)
        : patGrantRepositories(org, g.id),
  );
  const summary = summarizePermissions({
    repository: { metadata: "read", ...g.permissions.repository },
    organization: g.permissions.organization,
    other: g.permissions.other,
  });
  return (
    <li
      className={`${styles.listItem} ${local.patRow}`}
      data-token={g.token_name}
    >
      <KeyIcon size={16} className={styles.subtle} />
      <div className={local.patMain}>
        <span className={local.patTitle}>
          <strong>{g.token_name}</strong>
          <span className={styles.subtle}>by</span>
          <Avatar
            user={{ login: g.owner.login, avatarUrl: g.owner.avatar_url }}
            size={16}
          />
          <span>{g.owner.login}</span>
          {kind === "request" ? (
            <StatusPill status="warning">Pending</StatusPill>
          ) : g.token_expired ? (
            <StatusPill status="error">Expired</StatusPill>
          ) : (
            <StatusPill status="ok">Active</StatusPill>
          )}
        </span>
        {kind === "request" && g.reason && (
          <span className={styles.muted}>“{g.reason}”</span>
        )}
        <span className={styles.subtle}>{summary.join(" · ")}</span>
        <span className={`${styles.subtle} ${local.patMeta}`}>
          <span>
            {g.repository_selection === "subset" ? (
              <button
                type="button"
                className={local.linkButton}
                onClick={() => setShowRepos((s) => !s)}
                aria-expanded={showRepos}
              >
                Selected repositories
              </button>
            ) : (
              selectionText(g.repository_selection, undefined, org)
            )}
          </span>
          <span>
            {g.token_expires_at
              ? `${g.token_expired ? "Expired" : "Expires"} ${formatDateTime(g.token_expires_at)}`
              : "No expiration"}
          </span>
          <span>
            {g.token_last_used_at ? (
              <>
                Last used <RelativeTime date={g.token_last_used_at} />
              </>
            ) : (
              "Never used"
            )}
          </span>
          {kind === "request" && g.created_at && (
            <span>
              Requested <RelativeTime date={g.created_at} />
            </span>
          )}
          {kind === "grant" && g.access_granted_at && (
            <span>
              Approved <RelativeTime date={g.access_granted_at} />
            </span>
          )}
        </span>
        {showRepos && (
          <span
            className={`${styles.subtle} ${local.patMeta}`}
            aria-label="Repositories"
          >
            {repos.data
              ? repos.data.length
                ? repos.data.map((r) => (
                    <span key={r.id} className={local.patRepo}>
                      {r.private ? (
                        <LockIcon size={12} />
                      ) : (
                        <RepoIcon size={12} />
                      )}
                      {r.full_name}
                    </span>
                  ))
                : "No repositories"
              : repos.error
                ? errorMessage(repos.error)
                : "Loading…"}
          </span>
        )}
      </div>
      <span className={local.patActions}>{actions}</span>
    </li>
  );
}
