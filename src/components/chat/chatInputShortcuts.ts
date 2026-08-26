import {
  DEFAULT_CHAT_INPUT_SEND_SHORTCUT,
  type ChatInputSendShortcut,
} from "../../lib/chatInputSettings";

interface ChatInputShortcutEvent {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
}

export function shouldSubmitChatInput(
  event: ChatInputShortcutEvent,
  sendShortcut: ChatInputSendShortcut = DEFAULT_CHAT_INPUT_SEND_SHORTCUT,
): boolean {
  if (event.isComposing || event.key !== "Enter") {
    return false;
  }

  if (event.ctrlKey || event.metaKey) {
    return true;
  }

  return sendShortcut === "enter" ? !event.shiftKey : event.shiftKey;
}
