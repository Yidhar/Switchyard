import type { AttachmentKind, InputAttachment } from '../types';

export const ATTACHMENT_MARKER = '[Switchyard Attachments]';

const IMAGE_EXTENSIONS = new Set(['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp', 'tif', 'tiff']);

export function filenameFromPath(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function extensionFromPath(path: string): string {
  const filename = filenameFromPath(path);
  const dotIndex = filename.lastIndexOf('.');
  return dotIndex === -1 ? '' : filename.slice(dotIndex + 1).toLowerCase();
}

export function imageMimeTypeForPath(path: string): string | null {
  switch (extensionFromPath(path)) {
    case 'png':
      return 'image/png';
    case 'jpg':
    case 'jpeg':
      return 'image/jpeg';
    case 'webp':
      return 'image/webp';
    case 'gif':
      return 'image/gif';
    case 'bmp':
      return 'image/bmp';
    case 'tif':
    case 'tiff':
      return 'image/tiff';
    default:
      return null;
  }
}

export function isImageAttachmentPath(path: string): boolean {
  return IMAGE_EXTENSIONS.has(extensionFromPath(path));
}

export function attachmentKindForPath(path: string, mimeType?: string | null): AttachmentKind {
  if (mimeType?.toLowerCase().startsWith('image/')) return 'image';
  return isImageAttachmentPath(path) ? 'image' : 'file';
}

export function attachmentFromPath(path: string, mimeType?: string | null): InputAttachment {
  const kind = attachmentKindForPath(path, mimeType);
  return {
    path,
    name: filenameFromPath(path),
    kind,
    mimeType: mimeType ?? (kind === 'image' ? imageMimeTypeForPath(path) : null),
  };
}

function normalizeAttachmentPath(raw: string): string {
  return raw.trim().replace(/^["']|["']$/g, '');
}

export function stripAttachmentReferences(text: string): string {
  const markerIndex = text.indexOf(ATTACHMENT_MARKER);
  if (markerIndex === -1) return text;
  return text.slice(0, markerIndex).trimEnd();
}

export function extractAttachmentPathsFromAttachmentReferences(text: string): string[] {
  const markerIndex = text.indexOf(ATTACHMENT_MARKER);
  if (markerIndex === -1) return [];
  return text
    .slice(markerIndex + ATTACHMENT_MARKER.length)
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.startsWith('- '))
    .map((line) => normalizeAttachmentPath(line.slice(2).replace(/\s+\([^)]*\)\s*$/, '')))
    .filter(Boolean);
}

export function extractAttachmentsFromAttachmentReferences(text: string): InputAttachment[] {
  const markerIndex = text.indexOf(ATTACHMENT_MARKER);
  if (markerIndex === -1) return [];
  return text
    .slice(markerIndex + ATTACHMENT_MARKER.length)
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.startsWith('- '))
    .map((line) => {
      const withoutBullet = line.slice(2).trim();
      const mimeMatch = withoutBullet.match(/\s+\(([^)]*)\)\s*$/);
      const mimeType = mimeMatch?.[1]?.trim() || null;
      const path = normalizeAttachmentPath(withoutBullet.replace(/\s+\([^)]*\)\s*$/, ''));
      return path ? attachmentFromPath(path, mimeType) : null;
    })
    .filter((attachment): attachment is InputAttachment => Boolean(attachment));
}

export function extractImagePathsFromAttachmentReferences(text: string): string[] {
  return extractAttachmentsFromAttachmentReferences(text)
    .filter((attachment) => attachment.kind === 'image')
    .map((attachment) => attachment.path);
}

export function extractFilePathsFromAttachmentReferences(text: string): string[] {
  return extractAttachmentsFromAttachmentReferences(text)
    .filter((attachment) => attachment.kind !== 'image')
    .map((attachment) => attachment.path);
}

export function mergeInputAttachments(...groups: Array<InputAttachment[] | undefined | null>): InputAttachment[] {
  const seen = new Set<string>();
  const merged: InputAttachment[] = [];
  for (const group of groups) {
    for (const attachment of group ?? []) {
      const path = typeof attachment?.path === 'string' ? attachment.path.trim() : '';
      if (!path || seen.has(path)) continue;
      seen.add(path);
      merged.push({
        path,
        name: attachment.name || filenameFromPath(path),
        kind: attachment.kind || attachmentKindForPath(path, attachment.mimeType),
        mimeType: attachment.mimeType ?? (isImageAttachmentPath(path) ? imageMimeTypeForPath(path) : null),
      });
    }
  }
  return merged;
}
