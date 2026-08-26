import { GeminiImageCodec } from "./codec";

// Google adapter: renders through the shared codec.
export class GoogleImageCodec extends GeminiImageCodec {
  render(raw: string): string {
    return this.decode(raw);
  }
}
