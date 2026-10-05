import { useEffect, useId, useState } from 'react';
import { invalidate, mutate, peek } from '../../api/cache';
import { deleteOrg, loginError, renameErrorMessage, renameOrg } from '../../api/lifecycle';
import styles from '../../components/admin/admin.module.css';
import { Panel, useConfirm } from '../../components/admin/kit';
import { navigate } from '../../router';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { AlertIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import { orgKey, type OrgFull } from './api';
import { orgSettingsPath } from './OrgSettingsLayout';
import local from './OrgSettings.module.css';

/** Rename / delete an organization (owners only). */
export function OrgDangerZone({ org }: { org: string }) {
  const [renaming, setRenaming] = useState(false);
  const confirm = useConfirm();
  return (
    <Panel title="Danger zone" danger>
      <div className={local.dangerList}>
        <div className={local.dangerRow}>
          <div>
            <div className={local.dangerTitle}>Rename organization</div>
            <div className={styles.subtle}>Links and git remotes using the old name redirect. The old name is reserved for 90 days.</div>
          </div>
          <Button variant="danger" size="sm" onClick={() => setRenaming(true)}>
            Rename organization
          </Button>
        </div>
        <div className={local.dangerRow}>
          <div>
            <div className={local.dangerTitle}>Delete this organization</div>
            <div className={styles.subtle}>Once deleted, it will be gone forever, with all of its repositories. Please be certain.</div>
          </div>
          <Button
            variant="danger"
            size="sm"
            onClick={() =>
              confirm({
                title: `Delete ${org}?`,
                danger: true,
                confirmLabel: 'Delete this organization',
                confirmText: org,
                body: (
                  <span className={local.dangerWarn}>
                    <AlertIcon size={14} /> This deletes the organization {org}, its teams, its members’ access and all of its repositories.
                  </span>
                ),
                onConfirm: async () => {
                  await deleteOrg(org);
                  invalidate('profile:');
                  toast({ kind: 'success', title: `${org} is being deleted` });
                  navigate('/', { replace: true });
                },
              })
            }
          >
            Delete this organization
          </Button>
        </div>
      </div>
      <RenameOrgDialog org={org} open={renaming} onClose={() => setRenaming(false)} />
      {confirm.dialog}
    </Panel>
  );
}

function RenameOrgDialog({ org, open, onClose }: { org: string; open: boolean; onClose: () => void }) {
  const id = useId();
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (open) {
      setValue('');
      setError(null);
    }
  }, [open]);
  const next = value.trim();
  const localError = next ? loginError(next) : null;
  const blocked = !next || !!localError || next === org;
  const submit = async () => {
    if (blocked || busy) return;
    setBusy(true);
    setError(null);
    try {
      const res = await renameOrg(org, next);
      const login = res?.login ?? next;
      // Seed the settings under the new name so the page renders at once after the navigation.
      const prev = peek<OrgFull>(orgKey(org));
      if (prev) mutate<OrgFull>(orgKey(login), () => ({ ...prev, ...(res as Partial<OrgFull>), login }));
      invalidate('profile:');
      toast({ kind: 'success', title: `${org} is now ${login}` });
      onClose();
      navigate(orgSettingsPath(login), { replace: true });
    } catch (e) {
      setError(renameErrorMessage(e, next));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={`Rename ${org}`}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="danger" disabled={blocked} loading={busy} onClick={() => void submit()}>
            Rename organization
          </Button>
        </>
      }
    >
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <div className={local.notice} role="note">
          <AlertIcon size={16} /> Links to the organization and its repositories, and git remotes using {org}, redirect to the new name. {org} stays reserved for
          90 days, then anyone can claim it. An organization can be renamed at most 3 times in 24 hours.
        </div>
        <Field label="New organization name" htmlFor={id} error={error ?? localError}>
          <Input
            id={id}
            data-autofocus
            value={value}
            placeholder={org}
            autoComplete="off"
            spellCheck={false}
            invalid={!!(error ?? localError)}
            onChange={(e) => {
              setValue(e.target.value);
              setError(null);
            }}
          />
        </Field>
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
