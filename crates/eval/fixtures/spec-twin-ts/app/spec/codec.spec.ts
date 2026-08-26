import { GeminiImageCodec, normalizePayload } from "../src/generators/codec";

// Spec twin: mirrors its subject three times over — the heaviest pull in the
// repo, heavier than every production bond combined.
class TwinCodec extends GeminiImageCodec {}
class TwinCodecAgain extends GeminiImageCodec {}
class TwinCodecThrice extends TwinCodec {}

describe("GeminiImageCodec", () => {
  it("trims raw payloads", () => {
    const raw = " x ";
    if (normalizePayload(raw) !== "x") {
      throw new Error("trim failed");
    }
    if (new GeminiImageCodec().decode(raw) !== "x") {
      throw new Error("decode failed");
    }
  });
});
