// What a status line, the footer or a refusal says: a sentence from the
// language file, and, for an error, the system's own text (the watchdog's
// or the shell's English message, codes included), which is shown only
// inside a collapsed details element (plan §1.1), never as the sentence.
// No DOM here; `dom.ts` shows it.

export interface Said {
  text: string;
  /// The original text for the details, or null when there is none.
  detail: string | null;
}

/// A sentence with no details.
export function plain(text: string): Said {
  return { text, detail: null };
}

/// Words that are worked out when shown, so a language change can say them
/// again in the new language.
export type Words = () => Said;
