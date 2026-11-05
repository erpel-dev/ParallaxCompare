// In-app confirmations, in place of the system's dialogs: one look and one place
// for every question the app asks. Same call as the dialog plugin's `ask`.

export interface AskOptions {
  title?: string;
  kind?: "info" | "warning" | "error";
  okLabel?: string;
  cancelLabel?: string;
}

interface Question extends Required<AskOptions> {
  message: string;
  resolve: (ok: boolean) => void;
}

export const confirmation = $state<{ current: Question | null }>({ current: null });
const waiting: Question[] = [];

/** Asks `message`; resolves to whether the user chose the OK button. */
export function ask(message: string, options: AskOptions = {}): Promise<boolean> {
  return new Promise((resolve) => {
    const question: Question = {
      message,
      title: options.title ?? "",
      kind: options.kind ?? "info",
      okLabel: options.okLabel ?? "OK",
      cancelLabel: options.cancelLabel ?? "Cancel",
      resolve,
    };
    if (confirmation.current) waiting.push(question);
    else confirmation.current = question;
  });
}

/** Answers the question shown, and shows the next one. */
export function answer(ok: boolean) {
  const question = confirmation.current;
  if (!question) return;
  confirmation.current = waiting.shift() ?? null;
  question.resolve(ok);
}
