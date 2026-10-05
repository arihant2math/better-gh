import { afterEach, describe, expect, it } from 'vitest';
import { ApiError, SUDO_REQUIRED_PREFIX, api, isSudoRequired, setSudoHandler } from './client';
import { browserTransport, setTransport } from './transport';
import { b64urlDecode, b64urlEncode, toCreationOptions, toRequestOptions } from './webauthn';

describe('webauthn helpers', () => {
  it('round-trips base64url', () => {
    const bytes = new Uint8Array([0, 1, 2, 250, 251, 252, 253, 254, 255]);
    const s = b64urlEncode(bytes);
    expect(s).not.toMatch(/[+/=]/);
    expect([...b64urlDecode(s)]).toEqual([...bytes]);
    expect(b64urlEncode(new Uint8Array([]))).toBe('');
    expect([...b64urlDecode('-_8')]).toEqual([251, 255]);
  });

  it('converts webauthn-rs options to navigator.credentials shapes', () => {
    const create = toCreationOptions({
      publicKey: {
        rp: { id: 'localhost', name: 'Better GitHub' },
        user: { id: b64urlEncode(new Uint8Array([9, 9])), name: 'ada', displayName: 'Ada' },
        challenge: b64urlEncode(new Uint8Array([1, 2, 3])),
        pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
        excludeCredentials: [{ type: 'public-key', id: b64urlEncode(new Uint8Array([7])) }],
        authenticatorSelection: { residentKey: 'required', requireResidentKey: true, userVerification: 'required' },
        extensions: { credProps: true, uvm: true, credentialProtectionPolicy: 'userVerificationRequired', hmacCreateSecret: null },
      },
    });
    expect([...(create.challenge as Uint8Array)]).toEqual([1, 2, 3]);
    expect([...(create.user.id as Uint8Array)]).toEqual([9, 9]);
    expect([...(create.excludeCredentials![0]!.id as Uint8Array)]).toEqual([7]);
    expect(create.authenticatorSelection?.residentKey).toBe('required');
    expect(create.extensions).toEqual({ credProps: true, uvm: true, credentialProtectionPolicy: 'userVerificationRequired' });

    const get = toRequestOptions({
      publicKey: { challenge: 'AQ', rpId: 'localhost', allowCredentials: [], userVerification: 'preferred' },
      mediation: null,
    });
    expect([...(get.challenge as Uint8Array)]).toEqual([1]);
    expect(get.allowCredentials).toEqual([]);
    expect(get.rpId).toBe('localhost');
  });
});

describe('sudo prompt', () => {
  afterEach(() => setTransport(browserTransport));

  const sudo401 = () =>
    new Response(JSON.stringify({ message: `${SUDO_REQUIRED_PREFIX}: confirm your password.` }), { status: 401, headers: { 'Content-Type': 'application/json' } });

  it('asks for re-authentication and retries the request once', async () => {
    let calls = 0;
    setTransport({
      fetch: async () => (++calls === 1 ? sudo401() : new Response(JSON.stringify({ ok: true }), { status: 201, headers: { 'Content-Type': 'application/json' } })),
      socket: () => {
        throw new Error('no sockets');
      },
    });
    let prompts = 0;
    const uninstall = setSudoHandler(async () => (prompts++, true));
    await expect(api.post('/_bgh/tokens', { name: 'x' })).resolves.toEqual({ ok: true });
    expect(prompts).toBe(1);
    expect(calls).toBe(2);
    uninstall();
  });

  it('surfaces the 401 when the prompt is cancelled or absent', async () => {
    setTransport({
      fetch: async () => sudo401(),
      socket: () => {
        throw new Error('no sockets');
      },
    });
    const uninstall = setSudoHandler(async () => false);
    const err = await api.post('/_bgh/tokens', {}).catch((e: unknown) => e);
    expect(isSudoRequired(err)).toBe(true);
    uninstall();
    const again = await api.post('/_bgh/tokens', {}).catch((e: unknown) => e);
    expect(again).toBeInstanceOf(ApiError);
    expect(isSudoRequired(new ApiError('Bad credentials', 401, null))).toBe(false);
  });
});
