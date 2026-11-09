import { describe, expect, it } from "vitest";
import { getBuildSemver, getVersionInfo, resolveRuntimeReleaseIdentity } from "./version.js";

describe("resolveRuntimeReleaseIdentity", () => {
  it("binds injected release metadata to the package version", () => {
    const release = resolveRuntimeReleaseIdentity({
      packageVersion: "3.2.0",
      deployMode: "self-host",
      env: {
        PP_VERSION: "3.2.0-web",
        PP_COMMIT: "a".repeat(40),
        PP_TAG: "v3.2.0",
        PP_BUILD_DATE: "2026-08-20T12:00:00Z",
      },
    });

    expect(release).toEqual({
      version: "3.2.0",
      runtime_version: "3.2.0-web",
      commit: "a".repeat(40),
      tag: "v3.2.0",
      image_digest: null,
      deployment_mode: "self-host",
      github_release_url: "https://github.com/Print-Partner/PrintPartner/releases/tag/v3.2.0",
      build_date: "2026-08-20T12:00:00Z",
    });
    expect(getBuildSemver(release)).toBe(`v3.2.0+${"a".repeat(7)}`);
    expect(getVersionInfo(release).commit).toBe("a".repeat(7));
  });

  it("marks source builds as development identities", () => {
    const release = resolveRuntimeReleaseIdentity({
      packageVersion: "3.2.0",
      deployMode: "self-host",
      env: {},
    });

    expect(release.runtime_version).toBe("3.2.0-web");
    expect(release.commit).toBeNull();
    expect(release.tag).toBeNull();
    expect(getBuildSemver(release)).toBe("v3.2.0-dev");
  });

  it("rejects injected versions and tags that disagree with the package", () => {
    expect(() =>
      resolveRuntimeReleaseIdentity({
        packageVersion: "3.2.0",
        deployMode: "self-host",
        env: { PP_VERSION: "3.1.0-web" },
      }),
    ).toThrow(/PP_VERSION/);
    expect(() =>
      resolveRuntimeReleaseIdentity({
        packageVersion: "3.2.0",
        deployMode: "self-host",
        env: { PP_TAG: "v3.1.0" },
      }),
    ).toThrow(/PP_TAG/);
  });
});
