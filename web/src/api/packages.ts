/**
 * Packages (container registry) — GitHub REST shapes plus the private
 * `/_bgh/packages` web endpoints the UI reads (package P15).
 */
import { api, v3 } from './client';

export type PackageVisibility = 'public' | 'private' | 'internal';

export interface PackageOwner {
  login: string;
  id: number;
  avatar_url: string;
  type: 'User' | 'Organization' | string;
  html_url?: string;
}

export interface PackageRepo {
  name: string;
  full_name: string;
  html_url: string;
  private: boolean;
  owner: { login: string };
}

/** GitHub `package` object. */
export interface RestPackage {
  id: number;
  name: string;
  package_type: string;
  owner: PackageOwner;
  version_count: number;
  visibility: PackageVisibility;
  url: string;
  html_url: string;
  created_at: string;
  updated_at: string;
  repository?: PackageRepo | null;
}

/** GitHub `package-version` object. */
export interface RestPackageVersion {
  id: number;
  name: string;
  url: string;
  package_html_url: string;
  html_url?: string;
  license: string | null;
  description: string | null;
  created_at: string;
  updated_at: string;
  metadata?: { package_type: string; container?: { tags: string[] } };
}

export interface PackageSummary extends RestPackage {
  size: number;
  latest: { id: number; tags: string[]; created_at: string } | null;
}

export interface VersionSummary extends RestPackageVersion {
  size: number;
  digest: string;
  media_type: string;
  platforms: string[];
}

export interface OwnerPackages {
  owner: { login: string; type: string };
  registry: string;
  packages: PackageSummary[];
}

export interface RepoPackages {
  registry: string;
  packages: PackageSummary[];
}

export interface PackageDetail {
  registry: string;
  package: RestPackage;
  size: number;
  viewer_can_write: boolean;
  viewer_can_admin: boolean;
  versions: VersionSummary[];
}

export interface PackagePatch {
  visibility?: 'public' | 'private';
  repository?: string | null;
}

// ---------------------------------------------------------------- pure helpers

/** Tags of a version (`metadata.container.tags`). */
export function versionTags(v: Pick<RestPackageVersion, 'metadata'>): string[] {
  return v.metadata?.container?.tags ?? [];
}

/** `sha256:0123456789ab…` → `0123456789ab` (12 hex chars, like `docker images`). */
export function shortDigest(digest: string): string {
  const hex = digest.includes(':') ? digest.slice(digest.indexOf(':') + 1) : digest;
  return hex.slice(0, 12);
}

/**
 * `docker pull` command for a tag (or a digest when the version is untagged).
 * Registry paths are lowercase (OCI distribution spec).
 */
export function pullCommand(registry: string, owner: string, name: string, ref: { tag?: string | null; digest?: string | null } = {}): string {
  const image = `${registry}/${owner.toLowerCase()}/${name.toLowerCase()}`;
  if (ref.tag) return `docker pull ${image}:${ref.tag}`;
  if (ref.digest) return `docker pull ${image}@${ref.digest}`;
  return `docker pull ${image}:latest`;
}

/** Web path of a package's page (GitHub's `/users|orgs/:owner/packages/:type/package/:name`). */
export function packageHref(p: { owner: Pick<PackageOwner, 'login' | 'type'>; package_type: string; name: string }): string {
  const scope = p.owner.type === 'Organization' ? 'orgs' : 'users';
  return `/${scope}/${encodeURIComponent(p.owner.login)}/packages/${encodeURIComponent(p.package_type)}/package/${encodeURIComponent(p.name)}`;
}

/** Web path of an owner's package list. */
export function packagesHref(owner: { login: string; type: string }): string {
  return `/${owner.type === 'Organization' ? 'orgs' : 'users'}/${encodeURIComponent(owner.login)}/packages`;
}

/** REST path of a package (`/api/v3/users|orgs/{owner}/packages/{type}/{name}`). */
export function packageRestPath(p: { owner: Pick<PackageOwner, 'login' | 'type'>; package_type: string; name: string }): string {
  return v3(p.owner.type === 'Organization' ? 'orgs' : 'users', p.owner.login, 'packages', p.package_type, p.name);
}

/** Tag shown by default in the install box: `latest` when present, else the newest version's first tag. */
export function defaultTag(versions: Pick<VersionSummary, 'metadata'>[]): string | null {
  const all = versions.flatMap(versionTags);
  if (all.includes('latest')) return 'latest';
  return all[0] ?? null;
}

// ---------------------------------------------------------------- requests

const enc = encodeURIComponent;

export const packageKeys = {
  owner: (owner: string) => `packages:owner:${owner.toLowerCase()}`,
  repo: (owner: string, repo: string) => `packages:repo:${owner.toLowerCase()}/${repo.toLowerCase()}`,
  detail: (owner: string, type: string, name: string) => `packages:detail:${owner.toLowerCase()}/${type}/${name}`,
};

export function listOwnerPackages(owner: string): Promise<OwnerPackages> {
  return api.get(`/_bgh/packages/${enc(owner)}`);
}

export function listRepoPackages(owner: string, repo: string): Promise<RepoPackages> {
  return api.get(`/_bgh/repos/${enc(owner)}/${enc(repo)}/packages`);
}

export function getPackage(owner: string, type: string, name: string): Promise<PackageDetail> {
  return api.get(`/_bgh/packages/${enc(owner)}/${enc(type)}/${enc(name)}`);
}

export function updatePackage(owner: string, type: string, name: string, patch: PackagePatch): Promise<PackageDetail> {
  return api.patch(`/_bgh/packages/${enc(owner)}/${enc(type)}/${enc(name)}`, patch);
}

export function deletePackage(p: RestPackage): Promise<null> {
  return api.delete(packageRestPath(p));
}

export function deletePackageVersion(p: RestPackage, versionId: number): Promise<null> {
  return api.delete(`${packageRestPath(p)}/versions/${versionId}`);
}
