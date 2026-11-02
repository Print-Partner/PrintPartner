import { describe, expect, it } from "vitest";
import {
  parseArrangeRequest,
  parseBasis,
  parseInitializeRequest,
  parseMoveRequest,
  parsePinRequest,
  parseRestoreRequest,
  parseToken,
  parseTransferRequest,
  plateCoordinate,
} from "./accepted-plates-route-model.js";

const digest = "a".repeat(64);
const mappingDigest = "b".repeat(64);
const token = "ppu_0123456789abcdef0123456789abcdef";

const expected = {
  profile_id: 7,
  plan_version: 3,
  plan_revision_id: 11,
  plan_revision_digest: digest,
  required_unit_mapping_digest: mappingDigest,
};

const revisionBody = {
  expected,
  expected_plate_revision_id: 5,
};

describe("accepted plates route model", () => {
  it("parses plate coordinates", () => {
    expect(plateCoordinate(0)).toBe(0);
    expect(plateCoordinate(-1)).toBeNull();
  });

  it("parses accepted plan basis and unit tokens", () => {
    expect(parseBasis(expected)).toEqual({
      profileId: 7,
      planVersion: 3,
      revisionId: 11,
      revisionDigest: digest,
      requiredUnitMappingDigest: mappingDigest,
    });
    expect(parseBasis({ ...expected, plan_revision_digest: "bad" })).toBeNull();
    expect(parseToken(token)).toBe(token);
    expect(parseToken("bad-token")).toBeNull();
  });

  it("parses initialize requests", () => {
    expect(
      parseInitializeRequest({
        expected,
        expected_plate_revision_id: null,
        assignments: [{ token, printer_id: " printer-1 " }],
      }),
    ).toMatchObject({
      expectedPlateRevisionId: null,
      assignments: [{ token, printerId: "printer-1" }],
    });
    expect(
      parseInitializeRequest({
        expected,
        expected_plate_revision_id: null,
        assignments: [{ token, printer_id: "" }],
      }),
    ).toBeNull();
  });

  it("parses move and revision-based requests", () => {
    expect(parseMoveRequest({ ...revisionBody, x_um: 0, y_um: 10 })).toMatchObject({
      expectedPlateRevisionId: 5,
      xUm: 0,
      yUm: 10,
    });
    expect(parseMoveRequest({ ...revisionBody, x_um: -1, y_um: 10 })).toBeNull();
    expect(parsePinRequest({ ...revisionBody, pinned: true })).toMatchObject({ pinned: true });
    expect(parseArrangeRequest({ ...revisionBody, mode: "all" })).toMatchObject({ mode: "all" });
    expect(parseRestoreRequest({ ...revisionBody, restore_plate_revision_id: 9 })).toMatchObject({
      restorePlateRevisionId: 9,
    });
  });

  it("parses mutually exclusive transfer targets", () => {
    expect(
      parseTransferRequest({
        ...revisionBody,
        target_plate_id: "plate_0123456789abcdef0123456789abcdef",
      }),
    ).toMatchObject({ targetPlateId: "plate_0123456789abcdef0123456789abcdef" });
    expect(parseTransferRequest({ ...revisionBody, target_printer_id: "printer-1" })).toMatchObject(
      {
        targetPrinterId: "printer-1",
      },
    );
    expect(
      parseTransferRequest({
        ...revisionBody,
        target_plate_id: "plate_0123456789abcdef0123456789abcdef",
        target_printer_id: "printer-1",
      }),
    ).toBeNull();
  });
});
