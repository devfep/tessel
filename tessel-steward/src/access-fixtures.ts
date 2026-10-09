import type { AccessConfig } from "./access";

export const TEAM = "acme.cloudflareaccess.com";
export const AUD = "aud-tag-1";
export const VIEWER = "felix@example.com";
export const NOW_MS = 1_790_000_000_000;
export const NOW_S = NOW_MS / 1000;
export const CONFIG: AccessConfig = { teamDomain: TEAM, audience: AUD, viewers: [VIEWER] };

export type Claims = Record<string, unknown>;

/** Claims of a token that passes every check at `NOW_MS`. */
export function validClaims(overrides: Claims = {}): Claims {
  return {
    iss: `https://${TEAM}`,
    aud: [AUD],
    email: VIEWER,
    exp: NOW_S + 600,
    nbf: NOW_S - 60,
    ...overrides,
  };
}

function toBase64Url(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replaceAll("=", "");
}

function encodeJson(value: unknown): string {
  return toBase64Url(new TextEncoder().encode(JSON.stringify(value)));
}

/** A locally generated RSA key that signs tokens and publishes itself as the team's certs. */
export class TestSigner {
  private constructor(
    private readonly privateKey: CryptoKey,
    readonly publicJwk: JsonWebKey,
    readonly kid: string,
  ) {}

  static async create(kid = "kid-1"): Promise<TestSigner> {
    const pair = (await crypto.subtle.generateKey(
      {
        name: "RSASSA-PKCS1-v1_5",
        modulusLength: 2048,
        publicExponent: new Uint8Array([1, 0, 1]),
        hash: "SHA-256",
      },
      true,
      ["sign", "verify"],
    )) as CryptoKeyPair;
    const publicJwk = (await crypto.subtle.exportKey("jwk", pair.publicKey)) as JsonWebKey;
    return new TestSigner(pair.privateKey, publicJwk, kid);
  }

  async sign(claims: Claims, header: Claims = {}): Promise<string> {
    const head = encodeJson({ alg: "RS256", kid: this.kid, typ: "JWT", ...header });
    const signingInput = `${head}.${encodeJson(claims)}`;
    const signature = await crypto.subtle.sign(
      "RSASSA-PKCS1-v1_5",
      this.privateKey,
      new TextEncoder().encode(signingInput),
    );
    return `${signingInput}.${toBase64Url(new Uint8Array(signature))}`;
  }

  /** A stand-in for `fetch` that serves this key at the team's certs URL and records the URLs. */
  certsFetch(urls: string[] = []): typeof fetch {
    const body = { keys: [{ ...this.publicJwk, kid: this.kid }], public_cert: {} };
    return ((input: RequestInfo | URL) => {
      const url = String(input);
      urls.push(url);
      if (url !== `https://${TEAM}/cdn-cgi/access/certs`) {
        return Promise.resolve(new Response("not found", { status: 404 }));
      }
      return Promise.resolve(Response.json(body));
    }) as typeof fetch;
  }
}
