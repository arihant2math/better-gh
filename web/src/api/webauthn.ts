/**
 * WebAuthn security keys and passkeys, and sudo mode (bgh-accounts
 * `webauthn.rs` / `security.rs`, docs/packages/p36-account-security.md).
 *
 * The server sends ceremony options as webauthn-rs JSON (`{publicKey}` with
 * base64url binary fields); these helpers turn them into the
 * `navigator.credentials` shapes and the resulting credentials back into
 * JSON. No dependency: the conversion is a few lines and `toJSON()` /
 * `parse*OptionsFromJSON` aren't available everywhere yet.
 */
import { api } from './client';

// ------------------------------------------------------------------ base64url

export function b64urlEncode(buf: ArrayBuffer | Uint8Array): string {
  const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export function b64urlDecode(s: string): Uint8Array<ArrayBuffer> {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/');
  const bin = atob(b64 + '='.repeat((4 - (b64.length % 4)) % 4));
  const out = new Uint8Array(new ArrayBuffer(bin.length));
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// ------------------------------------------------------------------ option conversion

interface JsonDescriptor {
  type: string;
  id: string;
  transports?: string[];
}

export interface CreationOptionsJson {
  publicKey: {
    rp: { id?: string; name: string };
    user: { id: string; name: string; displayName: string };
    challenge: string;
    pubKeyCredParams: { type: string; alg: number }[];
    timeout?: number;
    excludeCredentials?: JsonDescriptor[];
    authenticatorSelection?: AuthenticatorSelectionCriteria;
    attestation?: AttestationConveyancePreference;
    extensions?: Record<string, unknown>;
  };
}

export interface RequestOptionsJson {
  publicKey: {
    challenge: string;
    timeout?: number;
    rpId?: string;
    allowCredentials?: JsonDescriptor[];
    userVerification?: UserVerificationRequirement;
    extensions?: Record<string, unknown>;
  };
  mediation?: CredentialMediationRequirement | null;
}

function descriptor(d: JsonDescriptor): PublicKeyCredentialDescriptor {
  return {
    type: d.type as PublicKeyCredentialType,
    id: b64urlDecode(d.id),
    transports: d.transports as AuthenticatorTransport[] | undefined,
  };
}

/** Only extensions browsers understand without binary inputs. */
function extensions(ext?: Record<string, unknown>): AuthenticationExtensionsClientInputs | undefined {
  if (!ext) return undefined;
  const out: Record<string, unknown> = {};
  for (const k of ['credProps', 'uvm', 'credentialProtectionPolicy', 'enforceCredentialProtectionPolicy']) if (k in ext && ext[k] != null) out[k] = ext[k];
  return out;
}

export function toCreationOptions(o: CreationOptionsJson): PublicKeyCredentialCreationOptions {
  const p = o.publicKey;
  return {
    rp: p.rp,
    user: { ...p.user, id: b64urlDecode(p.user.id) },
    challenge: b64urlDecode(p.challenge),
    pubKeyCredParams: p.pubKeyCredParams as PublicKeyCredentialParameters[],
    timeout: p.timeout,
    excludeCredentials: p.excludeCredentials?.map(descriptor),
    authenticatorSelection: p.authenticatorSelection,
    attestation: p.attestation,
    extensions: extensions(p.extensions),
  };
}

export function toRequestOptions(o: RequestOptionsJson): PublicKeyCredentialRequestOptions {
  const p = o.publicKey;
  return {
    challenge: b64urlDecode(p.challenge),
    timeout: p.timeout,
    rpId: p.rpId,
    allowCredentials: p.allowCredentials?.map(descriptor),
    userVerification: p.userVerification,
    extensions: extensions(p.extensions),
  };
}

/** A new credential as webauthn-rs `RegisterPublicKeyCredential` JSON. */
export function attestationToJson(c: PublicKeyCredential): unknown {
  const r = c.response as AuthenticatorAttestationResponse;
  return {
    id: c.id,
    rawId: b64urlEncode(c.rawId),
    type: c.type,
    response: {
      attestationObject: b64urlEncode(r.attestationObject),
      clientDataJSON: b64urlEncode(r.clientDataJSON),
      transports: typeof r.getTransports === 'function' ? r.getTransports() : undefined,
    },
    extensions: c.getClientExtensionResults?.() ?? {},
  };
}

/** An assertion as webauthn-rs `PublicKeyCredential` JSON. */
export function assertionToJson(c: PublicKeyCredential): unknown {
  const r = c.response as AuthenticatorAssertionResponse;
  return {
    id: c.id,
    rawId: b64urlEncode(c.rawId),
    type: c.type,
    response: {
      authenticatorData: b64urlEncode(r.authenticatorData),
      clientDataJSON: b64urlEncode(r.clientDataJSON),
      signature: b64urlEncode(r.signature),
      userHandle: r.userHandle ? b64urlEncode(r.userHandle) : null,
    },
    extensions: c.getClientExtensionResults?.() ?? {},
  };
}

export function webauthnSupported(): boolean {
  return typeof window !== 'undefined' && typeof window.PublicKeyCredential === 'function' && !!navigator.credentials;
}

/** Run a registration ceremony in the browser. */
export async function createCredential(options: CreationOptionsJson): Promise<unknown> {
  const cred = (await navigator.credentials.create({
    publicKey: toCreationOptions(options),
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error('No credential was created.');
  return attestationToJson(cred);
}

/** Run an assertion ceremony in the browser. */
export async function getAssertion(options: RequestOptionsJson): Promise<unknown> {
  const cred = (await navigator.credentials.get({
    publicKey: toRequestOptions(options),
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error('No credential was selected.');
  return assertionToJson(cred);
}

/** A friendlier message for the DOMExceptions WebAuthn throws. */
export function webauthnError(e: unknown): string {
  if (e instanceof DOMException) {
    if (e.name === 'NotAllowedError') return 'The request was cancelled or timed out.';
    if (e.name === 'InvalidStateError') return 'This security key is already registered.';
    if (e.name === 'SecurityError') return 'Security keys are not available on this address (HTTPS or localhost is required).';
  }
  return e instanceof Error ? e.message : 'Security key verification failed.';
}

// ------------------------------------------------------------------ endpoints

export type CredentialKind = 'security_key' | 'passkey';

export interface WebauthnCredential {
  id: number;
  name: string;
  kind: CredentialKind;
  created_at: string;
  last_used_at: string | null;
}

export interface Challenge<T> {
  id: string;
  options: T;
}

export const listCredentials = () => api.get<WebauthnCredential[]>('/_bgh/user/webauthn');
export const renameCredential = (id: number, name: string) => api.patch<WebauthnCredential>(`/_bgh/user/webauthn/${id}`, { name });
export const deleteCredential = (id: number) => api.delete<null>(`/_bgh/user/webauthn/${id}`);

/** Register a security key or passkey: server options → browser → server. */
export async function registerCredential(kind: CredentialKind, name: string): Promise<WebauthnCredential> {
  const ch = await api.post<Challenge<CreationOptionsJson>>('/_bgh/user/webauthn/registrations', { kind });
  const credential = await createCredential(ch.options);
  return api.post<WebauthnCredential>(`/_bgh/user/webauthn/registrations/${encodeURIComponent(ch.id)}`, { name, credential });
}

/** Passwordless sign-in; resolves to boot JSON (the cookie is set). */
export async function passkeySignIn<T>(): Promise<T> {
  const ch = await api.post<Challenge<RequestOptionsJson>>('/_bgh/auth/login/passkey/challenge');
  const credential = await getAssertion(ch.options);
  return api.post<T>('/_bgh/auth/login/passkey', { id: ch.id, credential });
}

/** Second factor of a pending password sign-in with a security key. */
export async function securityKeyTwoFactor<T>(twoFactorToken: string): Promise<T> {
  const ch = await api.post<Challenge<RequestOptionsJson>>('/_bgh/auth/2fa/webauthn/challenge', { twoFactorToken });
  const credential = await getAssertion(ch.options);
  return api.post<T>('/_bgh/auth/2fa/webauthn', {
    twoFactorToken,
    id: ch.id,
    credential,
  });
}

// ------------------------------------------------------------------ sudo mode

export interface SudoStatus {
  active: boolean;
  expires_at: string | null;
  methods: { password: boolean; totp: boolean; webauthn: boolean };
}

export const getSudo = () => api.get<SudoStatus>('/_bgh/sudo');
export const sudoWithPassword = (password: string) => api.post<SudoStatus>('/_bgh/sudo', { password }, { noSudoPrompt: true });
export const sudoWithCode = (otp: string) => api.post<SudoStatus>('/_bgh/sudo', { otp }, { noSudoPrompt: true });
export async function sudoWithSecurityKey(): Promise<SudoStatus> {
  const ch = await api.post<Challenge<RequestOptionsJson>>('/_bgh/sudo/webauthn/challenge', undefined, { noSudoPrompt: true });
  const credential = await getAssertion(ch.options);
  return api.post<SudoStatus>('/_bgh/sudo', { webauthn: { id: ch.id, credential } }, { noSudoPrompt: true });
}
