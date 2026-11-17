export const MAX_ATTACHED_IMAGE_BYTES = 10 * 1024 * 1024;
export const MAX_ATTACHED_IMAGES = 10;

export interface Base64ImageAttachment {
  data: string;
  mimeType: string;
}

function isBase64DataChar(code: number): boolean {
  return (code >= 0x41 && code <= 0x5a)
    || (code >= 0x61 && code <= 0x7a)
    || (code >= 0x30 && code <= 0x39)
    || code === 0x2b
    || code === 0x2f;
}

export function getBase64DecodedByteLength(data: string): number | null {
  if (!data || data.length % 4 !== 0) return null;
  const padding = data.endsWith("==") ? 2 : data.endsWith("=") ? 1 : 0;
  const dataEnd = data.length - padding;
  for (let index = 0; index < dataEnd; index += 1) {
    if (!isBase64DataChar(data.charCodeAt(index))) return null;
  }
  for (let index = dataEnd; index < data.length; index += 1) {
    if (data[index] !== "=") return null;
  }
  return (data.length / 4) * 3 - padding;
}

export function isBase64ImageWithinLimits(value: unknown): value is Base64ImageAttachment {
  if (!value || typeof value !== "object") return false;
  const image = value as Partial<Base64ImageAttachment>;
  if (typeof image.data !== "string" || typeof image.mimeType !== "string" || !image.mimeType.startsWith("image/")) {
    return false;
  }
  const bytes = getBase64DecodedByteLength(image.data);
  return bytes !== null && bytes <= MAX_ATTACHED_IMAGE_BYTES;
}
