import { GeminiImageCodec } from "./codec";

// Batch adapter: renders through the shared codec too.
export class BatchImageCodec extends GeminiImageCodec {
  renderAll(raw: string): string {
    return this.decode(raw);
  }
}
