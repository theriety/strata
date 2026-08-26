// Shared image-response codec: every generator adapter speaks through it.
export class GeminiImageCodec {
  decode(raw: string): string {
    return raw.trim();
  }
}

// Payload hygiene shared by every adapter in this folder.
export function normalizePayload(raw: string): string {
  return raw.trim().toLowerCase();
}
