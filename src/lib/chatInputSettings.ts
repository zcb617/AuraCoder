export const CHAT_INPUT_SEND_SHORTCUTS = ["shift-enter", "enter"] as const;

export type ChatInputSendShortcut = (typeof CHAT_INPUT_SEND_SHORTCUTS)[number];

export const DEFAULT_CHAT_INPUT_SEND_SHORTCUT: ChatInputSendShortcut = "shift-enter";

export function isChatInputSendShortcut(value: string): value is ChatInputSendShortcut {
  return (CHAT_INPUT_SEND_SHORTCUTS as readonly string[]).includes(value);
}

export const CHAT_INPUT_MODES = ["default", "classic"] as const;

export type ChatInputMode = (typeof CHAT_INPUT_MODES)[number];

export const DEFAULT_CHAT_INPUT_MODE: ChatInputMode = "classic";

export function isChatInputMode(value: string): value is ChatInputMode {
  return (CHAT_INPUT_MODES as readonly string[]).includes(value);
}

export const MESSAGE_SEND_MODES = ["classic", "flexible"] as const;

export type MessageSendMode = (typeof MESSAGE_SEND_MODES)[number];

export const DEFAULT_MESSAGE_SEND_MODE: MessageSendMode = "flexible";

export function isMessageSendMode(value: string): value is MessageSendMode {
  return (MESSAGE_SEND_MODES as readonly string[]).includes(value);
}
