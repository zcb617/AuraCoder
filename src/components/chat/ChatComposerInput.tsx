import type {
  Dispatch,
  FormEvent,
  MutableRefObject,
  RefObject,
  SetStateAction,
} from "react";
import { useTranslation } from "react-i18next";
import { ChatSlashMenu, type SlashCommand } from "./ChatSlashMenu";
import { ChatThreadMentionMenu } from "./ChatThreadMentionMenu";
import { shouldSubmitChatInput } from "./chatInputShortcuts";
import type { ActiveSlashCommand } from "./ChatCommandPanel";
import type { ThreadRuntimeSelectionPatch } from "./chatPanelTypes";
import type { ChatInputSendShortcut, MessageSendMode } from "../../lib/chatInputSettings";
import type { Thread } from "../../types";

/** 聊天输入组件，负责承载主输入框、会话引用菜单和斜杠菜单的交互展示。 */
export interface ChatComposerInputProps {
  /** 当前聊天输入框中的草稿文本。 */
  input: string;
  /** 主输入框引用，用于菜单定位和光标交互。 */
  inputRef: RefObject<HTMLTextAreaElement | null>;
  /** 当前活动工作区标识，用于判断输入框是否可用。 */
  activeWorkspaceId: string | null;
  /** 当前输入框是否处于计划模式。 */
  activePlanMode: boolean;
  /** 当前计划模式状态，用于快捷键切换并持久化。 */
  planMode: boolean;
  /** 当前是否使用 OpenCode 引擎。 */
  isOpenCodeEngine: boolean;
  /** 当前会话是否正在流式处理。 */
  streaming: boolean;
  /** 当前流式回合是否允许发送引导消息。 */
  canSteerActiveTurn: boolean;
  /** 当前会话的消息发送模式。 */
  sessionMessageSendMode: MessageSendMode;
  /** 当前输入框的提交快捷键配置。 */
  sendShortcut: ChatInputSendShortcut;
  /** AuraCoder 会话引用菜单是否打开。 */
  threadMentionMenuOpen: boolean;
  /** 当前匹配到的 AuraCoder 会话候选项。 */
  threadMentionCandidates: Thread[];
  /** AuraCoder 会话引用菜单的活动项下标。 */
  threadMentionActiveIndex: number;
  /** 斜杠菜单是否打开。 */
  slashMenuOpen: boolean;
  /** 当前匹配到的斜杠菜单命令。 */
  filteredSlashCommands: SlashCommand[];
  /** 斜杠菜单的活动项下标。 */
  slashMenuActiveIndex: number;
  /** 当前打开的斜杠命令面板。 */
  activeCommandPanel: ActiveSlashCommand | null;
  /** 当前活动会话，用于计划模式运行时选择持久化。 */
  activeThread: Thread | null;
  /** 当前聊天会话标识，用于没有活动线程时持久化运行时选择。 */
  activeChatSessionId: string | null;
  /** 输入历史记录引用。 */
  inputHistoryRef: MutableRefObject<string[]>;
  /** 输入历史游标引用。 */
  inputHistCursorRef: MutableRefObject<number>;
  /** 输入框实时草稿引用。 */
  inputLiveDraftRef: MutableRefObject<string>;
  /** 更新聊天输入草稿。 */
  setInput: (draft: string) => void;
  /** 更新计划模式状态。 */
  setPlanMode: Dispatch<SetStateAction<boolean>>;
  /** 更新 AuraCoder 会话引用菜单显示状态。 */
  setThreadMentionMenuOpen: Dispatch<SetStateAction<boolean>>;
  /** 更新 AuraCoder 会话引用菜单活动项。 */
  setThreadMentionActiveIndex: Dispatch<SetStateAction<number>>;
  /** 更新斜杠菜单显示状态。 */
  setSlashMenuOpen: Dispatch<SetStateAction<boolean>>;
  /** 更新斜杠菜单活动项。 */
  setSlashMenuActiveIndex: Dispatch<SetStateAction<number>>;
  /** 更新当前斜杠命令面板。 */
  setActiveCommandPanel: Dispatch<SetStateAction<ActiveSlashCommand | null>>;
  /** 更新斜杠命令面板错误信息。 */
  setCommandPanelError: Dispatch<SetStateAction<string | null>>;
  /** 根据光标位置检测斜杠命令输入。 */
  handleSlashDetection: (value: string, cursorPos: number) => void;
  /** 根据光标位置检测 AuraCoder 会话引用输入。 */
  handleThreadMentionDetection: (value: string, cursorPos: number) => void;
  /** 处理 AuraCoder 会话候选项选择。 */
  handleThreadMentionSelect: (thread: Thread) => void;
  /** 处理斜杠命令选择。 */
  handleSlashCommandSelect: (commandId: string) => void;
  /** 提交当前聊天输入内容。 */
  onSubmit: (event: FormEvent) => void;
  /** 保存当前线程的运行时选择变更。 */
  saveThreadRuntimeSelectionPatch: (
    threadId: string | null | undefined,
    patch: ThreadRuntimeSelectionPatch,
  ) => Promise<boolean>;
}

/** 渲染聊天主输入框及其会话引用、斜杠命令菜单，并转发输入交互到聊天面板。 */
export function ChatComposerInput({
  input,
  inputRef,
  activeWorkspaceId,
  activePlanMode,
  planMode,
  isOpenCodeEngine,
  streaming,
  canSteerActiveTurn,
  sessionMessageSendMode,
  sendShortcut,
  threadMentionMenuOpen,
  threadMentionCandidates,
  threadMentionActiveIndex,
  slashMenuOpen,
  filteredSlashCommands,
  slashMenuActiveIndex,
  activeCommandPanel,
  activeThread,
  activeChatSessionId,
  inputHistoryRef,
  inputHistCursorRef,
  inputLiveDraftRef,
  setInput,
  setPlanMode,
  setThreadMentionMenuOpen,
  setThreadMentionActiveIndex,
  setSlashMenuOpen,
  setSlashMenuActiveIndex,
  setActiveCommandPanel,
  setCommandPanelError,
  handleSlashDetection,
  handleThreadMentionDetection,
  handleThreadMentionSelect,
  handleSlashCommandSelect,
  onSubmit,
  saveThreadRuntimeSelectionPatch,
}: ChatComposerInputProps) {
  const { t } = useTranslation("chat");

  return (
    <>
                <textarea
                  ref={inputRef}
                  rows={3}
                  value={input}
                  onChange={(e) => {
                    inputHistCursorRef.current = -1;
                    setInput(e.target.value);
                    handleSlashDetection(
                      e.target.value,
                      e.target.selectionStart ?? e.target.value.length,
                    );
                    handleThreadMentionDetection(
                      e.target.value,
                      e.target.selectionStart ?? e.target.value.length,
                    );
                  }}
                  onKeyDown={(e) => {
                    /* ── AuraCoder 会话 @ 菜单键盘导航 ── */
                    if (threadMentionMenuOpen && threadMentionCandidates.length > 0) {
                      if (e.key === "ArrowDown") {
                        e.preventDefault();
                        setThreadMentionActiveIndex((index) =>
                          Math.min(index + 1, threadMentionCandidates.length - 1),
                        );
                        return;
                      }
                      if (e.key === "ArrowUp") {
                        e.preventDefault();
                        setThreadMentionActiveIndex((index) => Math.max(index - 1, 0));
                        return;
                      }
                      if (e.key === "Enter" || e.key === "Tab") {
                        e.preventDefault();
                        const thread = threadMentionCandidates[
                          Math.min(threadMentionActiveIndex, threadMentionCandidates.length - 1)
                        ];
                        if (thread) handleThreadMentionSelect(thread);
                        return;
                      }
                      if (e.key === "Escape") {
                        e.preventDefault();
                        setThreadMentionMenuOpen(false);
                        return;
                      }
                    }
                    /* ── Slash menu keyboard nav ── */
                    if (slashMenuOpen) {
                      if (e.key === "ArrowDown") {
                        e.preventDefault();
                        setSlashMenuActiveIndex((i) =>
                          Math.min(i + 1, filteredSlashCommands.length - 1),
                        );
                        return;
                      }
                      if (e.key === "ArrowUp") {
                        e.preventDefault();
                        setSlashMenuActiveIndex((i) => Math.max(i - 1, 0));
                        return;
                      }
                      if (e.key === "Enter" || e.key === "Tab") {
                        e.preventDefault();
                        const cmd = filteredSlashCommands[Math.min(slashMenuActiveIndex, filteredSlashCommands.length - 1)];
                        if (cmd) handleSlashCommandSelect(cmd.id);
                        return;
                      }
                      if (e.key === "Escape") {
                        e.preventDefault();
                        setSlashMenuOpen(false);
                        return;
                      }
                    }
                    /* ── Command panel dismiss ── */
                    if (activeCommandPanel && e.key === "Escape") {
                      e.preventDefault();
                      setActiveCommandPanel(null);
                      setCommandPanelError(null);
                      return;
                    }
                    /* ── Input history cycling (Option+Up / Option+Down) ── */
                    if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
                      const history = inputHistoryRef.current;
                      if (history.length === 0) return;
                      e.preventDefault();
                      if (e.key === "ArrowUp") {
                        if (inputHistCursorRef.current === -1) {
                          inputLiveDraftRef.current = input;
                        }
                        const next = Math.min(inputHistCursorRef.current + 1, history.length - 1);
                        inputHistCursorRef.current = next;
                        setInput(history[next]);
                      } else {
                        const next = inputHistCursorRef.current - 1;
                        inputHistCursorRef.current = next;
                        if (next < 0) {
                          setInput(inputLiveDraftRef.current);
                        } else {
                          setInput(history[next]);
                        }
                      }
                      return;
                    }
                    if (shouldSubmitChatInput({
                      key: e.key,
                      ctrlKey: e.ctrlKey,
                      metaKey: e.metaKey,
                      shiftKey: e.shiftKey,
                      isComposing: e.nativeEvent.isComposing,
                    }, sendShortcut)) {
                      e.preventDefault();
                      if (streaming && !canSteerActiveTurn && sessionMessageSendMode !== "flexible") {
                        return;
                      }
                      void onSubmit(e);
                    }
                    if (e.shiftKey && e.key === "Tab") {
                      e.preventDefault();
                      if (activeWorkspaceId && !isOpenCodeEngine) {
                        const nextPlanMode = !planMode;
                        setPlanMode(nextPlanMode);
                        void saveThreadRuntimeSelectionPatch(
                          activeThread?.id ?? activeChatSessionId,
                          { planMode: nextPlanMode },
                        );
                      }
                    }
                  }}
                  placeholder={
                    activePlanMode
                      ? t("panel.placeholders.plan")
                      : t("panel.placeholders.chat")
                  }
                  disabled={!activeWorkspaceId}
                  style={{
                    width: "100%",
                    padding: "12px 14px",
                    background: "transparent",
                    color: "var(--text-1)",
                    fontSize: 13,
                    lineHeight: 1.6,
                    resize: "none",
                    fontFamily: "inherit",
                    caretColor: activePlanMode ? "var(--accent-2)" : "var(--accent)",
                  }}
                />

                <ChatThreadMentionMenu
                  visible={threadMentionMenuOpen && threadMentionCandidates.length > 0}
                  threads={threadMentionCandidates}
                  anchorRef={inputRef}
                  activeIndex={threadMentionActiveIndex}
                  onSelect={handleThreadMentionSelect}
                  onDismiss={() => setThreadMentionMenuOpen(false)}
                  onActiveChange={setThreadMentionActiveIndex}
                />

                {/* Slash command menu (portal) */}
                <ChatSlashMenu
                  visible={slashMenuOpen && filteredSlashCommands.length > 0}
                  commands={filteredSlashCommands}
                  anchorRef={inputRef}
                  activeIndex={slashMenuActiveIndex}
                  onSelect={handleSlashCommandSelect}
                  onDismiss={() => setSlashMenuOpen(false)}
                  onActiveChange={setSlashMenuActiveIndex}
                />
    </>
  );
}
