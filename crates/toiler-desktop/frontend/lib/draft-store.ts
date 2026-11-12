import {
  MAX_ATTACHED_IMAGES,
  isBase64ImageWithinLimits,
} from "./image-attachments";

export interface ChatDraftImage {
  data: string;
  mimeType: string;
}

export type ChatDraftCommandKind = "command" | "skill" | "mcp" | "plugin";

export interface ChatDraftCommand {
  name: string;
  kind: ChatDraftCommandKind;
}

export interface ChatDraftMention {
  text: string;
}

export interface ChatDraft {
  value: string;
  images: ChatDraftImage[];
  selectedCommand?: ChatDraftCommand;
  selectedMentions?: ChatDraftMention[];
}

const drafts = new Map<string, ChatDraft>();

function cloneDraft(draft: ChatDraft): ChatDraft {
  return {
    value: draft.value,
    images: draft.images.map((image) => ({ ...image })),
    ...(draft.selectedCommand && { selectedCommand: { ...draft.selectedCommand } }),
    ...(draft.selectedMentions?.length && { selectedMentions: draft.selectedMentions.map((mention) => ({ ...mention })) }),
  };
}

function isEmptyDraft(draft: ChatDraft): boolean {
  return !draft.value && draft.images.length === 0 && !draft.selectedCommand && !draft.selectedMentions?.length;
}

export function getDraft(key: string): ChatDraft | null {
  const draft = drafts.get(key);
  return draft ? cloneDraft(draft) : null;
}

export function setDraft(key: string, draft: ChatDraft): void {
  if (isEmptyDraft(draft)) {
    drafts.delete(key);
    return;
  }
  drafts.set(key, cloneDraft(draft));
}

export function clearDraft(key: string): void {
  drafts.delete(key);
}

export function mergeRestoredSubmissionText(submitted: string, current: string): string {
  if (!submitted.trim()) return current;
  if (!current.trim()) return submitted;
  return `${submitted}\n\n${current}`;
}

export function mergeRestoredSubmissionDraft(
  submittedText: string,
  submittedImages: ChatDraftImage[] | undefined,
  currentText: string,
  currentImages: ChatDraftImage[],
): ChatDraft {
  const images = [...(submittedImages ?? []), ...currentImages]
    .filter(isBase64ImageWithinLimits)
    .slice(0, MAX_ATTACHED_IMAGES)
    .map(({ data, mimeType }) => ({ data, mimeType }));

  return {
    value: mergeRestoredSubmissionText(submittedText, currentText),
    images,
  };
}

export function restoreDraftSubmission(
  key: string,
  text: string,
  images?: ChatDraftImage[],
): ChatDraft {
  const current = getDraft(key) ?? { value: "", images: [] };
  const restored = mergeRestoredSubmissionDraft(
    text,
    images,
    current.value,
    current.images,
  );
  setDraft(key, restored);
  return restored;
}

export function rekeyDraft(
  previousKey: string,
  nextKey: string,
  currentDraft?: ChatDraft,
): ChatDraft | null {
  if (previousKey === nextKey) return currentDraft ? cloneDraft(currentDraft) : getDraft(nextKey);

  const storedPrevious = getDraft(previousKey);
  const previous = currentDraft && !isEmptyDraft(currentDraft)
    ? cloneDraft(currentDraft)
    : (storedPrevious ?? (currentDraft ? cloneDraft(currentDraft) : null));
  const next = getDraft(nextKey);
  clearDraft(previousKey);
  if (!previous) return next;

  const merged = next
    ? {
        ...mergeRestoredSubmissionDraft(next.value, next.images, previous.value, previous.images),
        ...(previous.selectedCommand ?? next.selectedCommand
          ? { selectedCommand: previous.selectedCommand ?? next.selectedCommand }
          : {}),
        ...((next.selectedMentions?.length || previous.selectedMentions?.length)
          ? { selectedMentions: [...(next.selectedMentions ?? []), ...(previous.selectedMentions ?? [])] }
          : {}),
      }
    : previous;
  setDraft(nextKey, merged);
  return cloneDraft(merged);
}
