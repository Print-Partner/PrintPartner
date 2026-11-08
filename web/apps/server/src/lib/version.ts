import type { DeployMode, RuntimeReleaseIdentity } from "@print-partner/contracts";

const VERSION_PATTERN = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const COMMIT_PATTERN = /^[0-9a-f]{40}$/;

interface VersionInfo {
  version: string;
  commit: string;
  branch: string;
  tag: string;
  buildDate: string;
  nodeVersion: string;
}

type ReleaseEnvironment = Partial<
  Record<"PP_VERSION" | "PP_COMMIT" | "PP_TAG" | "PP_IMAGE_DIGEST" | "PP_BUILD_DATE", string>
>;

function optionalValue(value: string | undefined): string | null {
  const normalized = value?.trim();
  return normalized ? normalized : null;
}

function assertPackageVersion(version: string): void {
  if (!VERSION_PATTERN.test(version)) throw new Error(`Invalid package version: ${version}`);
}

export function resolveRuntimeReleaseIdentity(input: {
  packageVersion: string;
  deployMode: DeployMode;
  env: ReleaseEnvironment;
}): RuntimeReleaseIdentity {
  assertPackageVersion(input.packageVersion);
  const runtimeVersion = `${input.packageVersion}-web`;
  const injectedVersion = optionalValue(input.env.PP_VERSION);
  if (injectedVersion !== null && injectedVersion !== runtimeVersion) {
    throw new Error(
      `PP_VERSION ${injectedVersion} does not match package version ${runtimeVersion}`,
    );
  }

  const commit = optionalValue(input.env.PP_COMMIT);
  if (commit !== null && !COMMIT_PATTERN.test(commit)) {
    throw new Error("PP_COMMIT must be a full lowercase 40-character Git SHA");
  }

  const tag = optionalValue(input.env.PP_TAG);
  if (tag !== null && tag !== `v${input.packageVersion}`) {
    throw new Error(`PP_TAG ${tag} does not match package version v${input.packageVersion}`);
  }

  const imageDigest = optionalValue(input.env.PP_IMAGE_DIGEST);
  if (imageDigest !== null && !/^sha256:[0-9a-f]{64}$/.test(imageDigest)) {
    throw new Error("PP_IMAGE_DIGEST must be a sha256 OCI digest");
  }

  return {
    version: input.packageVersion,
    runtime_version: runtimeVersion,
    commit,
    tag,
    image_digest: imageDigest,
    deployment_mode: input.deployMode,
    github_release_url:
      tag === null ? null : `https://github.com/Print-Partner/PrintPartner/releases/tag/${tag}`,
    build_date: optionalValue(input.env.PP_BUILD_DATE),
  };
}

export function getVersionInfo(release: RuntimeReleaseIdentity): VersionInfo {
  return {
    version: release.version,
    commit: release.commit?.slice(0, 7) ?? "unknown",
    branch: release.tag === null ? "development" : "detached",
    tag: release.tag ?? "unknown",
    buildDate: release.build_date ?? "unknown",
    nodeVersion: process.version,
  };
}

export function getBuildSemver(release: RuntimeReleaseIdentity): string {
  if (release.tag === `v${release.version}` && release.commit !== null) {
    return `v${release.version}+${release.commit.slice(0, 7)}`;
  }
  return `v${release.version}-dev`;
}
