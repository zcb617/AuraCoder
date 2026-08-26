import { beforeEach, describe, expect, it } from "vitest";
import {
  DEFAULT_CHAT_INPUT_MODE,
  DEFAULT_CHAT_INPUT_SEND_SHORTCUT,
} from "../lib/chatInputSettings";
import { DEFAULT_MESSAGE_SEND_MODE } from "../lib/chatInputSettings";
import {
  getChatComposerSessionKey,
  useChatComposerStore,
} from "./chatComposerStore";
import { DEFAULT_LINK_OPEN_GESTURE } from "../lib/linkOpenSettings";

describe("chatComposerStore", () => {
  it("uses the defaults configured for the chat settings", () => {
    expect(DEFAULT_CHAT_INPUT_SEND_SHORTCUT).toBe("shift-enter");
    expect(DEFAULT_CHAT_INPUT_MODE).toBe("classic");
    expect(DEFAULT_MESSAGE_SEND_MODE).toBe("flexible");
    expect(DEFAULT_LINK_OPEN_GESTURE).toBe("click");
  });

  beforeEach(() => {
    useChatComposerStore.setState({
      runtimeByWorkspace: {},
      draftBySession: {},
      attachmentsBySession: {},
      textAnnotationsBySession: {},
      referencesBySession: {},
      pendingFlexibleMessagesBySession: {},
      pendingMessageSendModeByWorkspace: {},
      sendShortcut: DEFAULT_CHAT_INPUT_SEND_SHORTCUT,
      chatInputMode: DEFAULT_CHAT_INPUT_MODE,
      messageSendMode: DEFAULT_MESSAGE_SEND_MODE,
      messageSendModeByThread: {},
      linkOpenGesture: DEFAULT_LINK_OPEN_GESTURE,
    });
  });

  /*
   * 旧项目级测试完整保留如下，不参与编译；会话级测试覆盖相同生命周期并增加
   * 同工作区不同会话的隔离断言：
   *
   * beforeEach(() => {
   *   useChatComposerStore.setState({
   *     runtimeByWorkspace: {},
   *     draftByWorkspace: {},
   *     attachmentsByWorkspace: {},
   *     textAnnotationsByWorkspace: {},
   *     referencesByWorkspace: {},
   *     pendingFlexibleMessagesBySession: {},
   *     pendingMessageSendModeByWorkspace: {},
   *     sendShortcut: DEFAULT_CHAT_INPUT_SEND_SHORTCUT,
   *     chatInputMode: DEFAULT_CHAT_INPUT_MODE,
   *     messageSendMode: DEFAULT_MESSAGE_SEND_MODE,
   *     messageSendModeByThread: {},
   *     linkOpenGesture: DEFAULT_LINK_OPEN_GESTURE,
   *   });
   * });
   *
   * it("keeps a workspace draft while the chat panel is unmounted", () => {
   *   const store = useChatComposerStore.getState();
   *   store.setWorkspaceDraft("workspace-a", "Continue this thought");
   *   expect(useChatComposerStore.getState().draftByWorkspace).toEqual({
   *     "workspace-a": "Continue this thought",
   *   });
   *   useChatComposerStore.getState().clearWorkspaceDraft("workspace-a");
   *   expect(useChatComposerStore.getState().draftByWorkspace).toEqual({});
   * });
   *
   * it("keeps workspace attachments while the chat panel is unmounted", () => {
   *   const attachments = [
   *     {
   *       id: "clipboard-image",
   *       fileName: "clipboard.png",
   *       filePath: "C:/tmp/clipboard.png",
   *       sizeBytes: 42,
   *       mimeType: "image/png",
   *     },
   *   ];
   *   useChatComposerStore
   *     .getState()
   *     .setWorkspaceAttachments("workspace-a", attachments);
   *   expect(useChatComposerStore.getState().attachmentsByWorkspace).toEqual({
   *     "workspace-a": attachments,
   *   });
   *   useChatComposerStore.getState().clearWorkspaceAttachments("workspace-a");
   *   expect(useChatComposerStore.getState().attachmentsByWorkspace).toEqual({});
   * });
   *
   * it("keeps workspace references while the chat panel is unmounted", () => {
   *   const references = [
   *     { type: "skill" as const, name: "task-anchor", path: "/skills/task-anchor" },
   *     { type: "mention" as const, name: "Docs", path: "app://docs" },
   *   ];
   *   useChatComposerStore
   *     .getState()
   *     .setWorkspaceReferences("workspace-a", references);
   *   expect(useChatComposerStore.getState().referencesByWorkspace).toEqual({
   *     "workspace-a": references,
   *   });
   *   useChatComposerStore.getState().clearWorkspaceReferences("workspace-a");
   *   expect(useChatComposerStore.getState().referencesByWorkspace).toEqual({});
   * });
   */
  it("keeps a session draft while the chat panel is unmounted", () => {
    const store = useChatComposerStore.getState();
    const sessionKey = getChatComposerSessionKey("workspace-a", "thread-a");

    // 原项目级草稿测试已由会话级范围测试替代。
    store.setSessionDraft(sessionKey, "Continue this thought");

    expect(useChatComposerStore.getState().draftBySession).toEqual({
      "thread:thread-a": "Continue this thought",
    });

    useChatComposerStore.getState().clearSessionDraft(sessionKey);

    expect(useChatComposerStore.getState().draftBySession).toEqual({});
  });

  it("keeps session attachments while the chat panel is unmounted", () => {
    const sessionKey = getChatComposerSessionKey("workspace-a", "thread-a");
    const attachments = [
      {
        id: "clipboard-image",
        fileName: "clipboard.png",
        filePath: "C:/tmp/clipboard.png",
        sizeBytes: 42,
        mimeType: "image/png",
      },
    ];

    useChatComposerStore
      .getState()
      .setSessionAttachments(sessionKey, attachments);

    expect(useChatComposerStore.getState().attachmentsBySession).toEqual({
      "thread:thread-a": attachments,
    });

    useChatComposerStore.getState().clearSessionAttachments(sessionKey);

    expect(useChatComposerStore.getState().attachmentsBySession).toEqual({});
  });

  it("keeps session references while the chat panel is unmounted", () => {
    const sessionKey = getChatComposerSessionKey("workspace-a", "thread-a");
    const references = [
      { type: "skill" as const, name: "task-anchor", path: "/skills/task-anchor" },
      { type: "mention" as const, name: "Docs", path: "app://docs" },
    ];

    useChatComposerStore
      .getState()
      .setSessionReferences(sessionKey, references);

    expect(useChatComposerStore.getState().referencesBySession).toEqual({
      "thread:thread-a": references,
    });

    useChatComposerStore.getState().clearSessionReferences(sessionKey);

    expect(useChatComposerStore.getState().referencesBySession).toEqual({});
  });

  it("isolates unsent composer content between threads in the same workspace", () => {
    const sessionA = getChatComposerSessionKey("workspace-a", "thread-a");
    const sessionB = getChatComposerSessionKey("workspace-a", "thread-b");
    const attachmentsA = [{
      id: "attachment-a",
      fileName: "a.png",
      filePath: "C:/tmp/a.png",
      sizeBytes: 1,
      mimeType: "image/png",
    }];
    const attachmentsB = [{
      id: "attachment-b",
      fileName: "b.png",
      filePath: "C:/tmp/b.png",
      sizeBytes: 2,
      mimeType: "image/png",
    }];
    const annotationsA = [{ id: "annotation-a", selectedText: "A", comment: "comment A" }];
    const annotationsB = [{ id: "annotation-b", selectedText: "B", comment: "comment B" }];
    const referencesA = [{ type: "skill" as const, name: "skill-a", path: "/skills/a" }];
    const referencesB = [{ type: "mention" as const, name: "mention-b", path: "app://b" }];
    const store = useChatComposerStore.getState();

    store.setSessionDraft(sessionA, "draft A");
    store.setSessionAttachments(sessionA, attachmentsA);
    store.setSessionTextAnnotations(sessionA, annotationsA);
    store.setSessionReferences(sessionA, referencesA);
    store.setSessionDraft(sessionB, "draft B");
    store.setSessionAttachments(sessionB, attachmentsB);
    store.setSessionTextAnnotations(sessionB, annotationsB);
    store.setSessionReferences(sessionB, referencesB);

    expect(useChatComposerStore.getState().draftBySession).toEqual({
      [sessionA]: "draft A",
      [sessionB]: "draft B",
    });
    expect(useChatComposerStore.getState().attachmentsBySession).toEqual({
      [sessionA]: attachmentsA,
      [sessionB]: attachmentsB,
    });
    expect(useChatComposerStore.getState().textAnnotationsBySession).toEqual({
      [sessionA]: annotationsA,
      [sessionB]: annotationsB,
    });
    expect(useChatComposerStore.getState().referencesBySession).toEqual({
      [sessionA]: referencesA,
      [sessionB]: referencesB,
    });

    store.clearSessionDraft(sessionA);
    store.clearSessionAttachments(sessionA);
    store.clearSessionTextAnnotations(sessionA);
    store.clearSessionReferences(sessionA);

    const state = useChatComposerStore.getState();
    expect(state.draftBySession[sessionA]).toBeUndefined();
    expect(state.attachmentsBySession[sessionA]).toBeUndefined();
    expect(state.textAnnotationsBySession[sessionA]).toBeUndefined();
    expect(state.referencesBySession[sessionA]).toBeUndefined();
    expect(state.draftBySession[sessionB]).toBe("draft B");
    expect(state.attachmentsBySession[sessionB]).toEqual(attachmentsB);
    expect(state.textAnnotationsBySession[sessionB]).toEqual(annotationsB);
    expect(state.referencesBySession[sessionB]).toEqual(referencesB);
  });

  it("uses a stable temporary session key before a formal thread exists", () => {
    const temporaryKey = getChatComposerSessionKey("workspace-a", null);
    const threadKey = getChatComposerSessionKey("workspace-a", "thread-a");

    expect(temporaryKey).toBe("new:workspace-a");
    expect(getChatComposerSessionKey("workspace-a", undefined)).toBe("new:workspace-a");
    expect(temporaryKey).not.toBe(threadKey);

    useChatComposerStore.getState().setSessionDraft(temporaryKey, "temporary draft");
    useChatComposerStore.getState().setSessionDraft(threadKey, "thread draft");

    expect(useChatComposerStore.getState().draftBySession).toEqual({
      [temporaryKey]: "temporary draft",
      [threadKey]: "thread draft",
    });
  });

  it("updates the configured send shortcut", () => {
    useChatComposerStore.getState().setSendShortcut("enter");

    expect(useChatComposerStore.getState().sendShortcut).toBe("enter");
  });

  it("updates the configured chat input mode", () => {
    useChatComposerStore.getState().setChatInputMode("classic");

    expect(useChatComposerStore.getState().chatInputMode).toBe("classic");
  });

  it("updates the configured message send mode", () => {
    useChatComposerStore.getState().setMessageSendMode("flexible");

    expect(useChatComposerStore.getState().messageSendMode).toBe("flexible");
  });

  it("keeps a message send mode override for an existing thread", () => {
    useChatComposerStore.getState().setThreadMessageSendMode("thread-a", "flexible");

    expect(useChatComposerStore.getState().messageSendModeByThread).toEqual({
      "thread-a": "flexible",
    });
  });

  it("keeps the selected message send mode until a new thread is created", () => {
    useChatComposerStore
      .getState()
      .setPendingMessageSendMode("workspace-a", "flexible");

    expect(useChatComposerStore.getState().pendingMessageSendModeByWorkspace).toEqual({
      "workspace-a": "flexible",
    });

    useChatComposerStore.getState().clearPendingMessageSendMode("workspace-a");

    expect(useChatComposerStore.getState().pendingMessageSendModeByWorkspace).toEqual({});
  });

  it("isolates pending flexible messages between threads in the same workspace", () => {
    const firstMessage = {
      auracoderThreadId: "referenced-thread",
      id: "pending-message-a",
      text: "Wait until I finish the details.",
      attachments: [],
      references: [],
    };
    const secondMessage = {
      auracoderThreadId: null,
      id: "pending-message-b",
      text: "This belongs to another thread.",
      attachments: [],
      references: [],
    };

    useChatComposerStore
      .getState()
      .addPendingFlexibleMessage("thread:thread-a", firstMessage);
    useChatComposerStore
      .getState()
      .addPendingFlexibleMessage("thread:thread-b", secondMessage);

    expect(useChatComposerStore.getState().pendingFlexibleMessagesBySession).toEqual({
      "thread:thread-a": [firstMessage],
      "thread:thread-b": [secondMessage],
    });

    useChatComposerStore
      .getState()
      .clearPendingFlexibleMessages("thread:thread-a");

    expect(useChatComposerStore.getState().pendingFlexibleMessagesBySession).toEqual({
      "thread:thread-b": [secondMessage],
    });
  });

  it("removes only the selected pending flexible message", () => {
    const firstMessage = {
      auracoderThreadId: null,
      id: "pending-message-1",
      text: "First held message",
      attachments: [],
      references: [],
    };
    const secondMessage = {
      auracoderThreadId: null,
      id: "pending-message-2",
      text: "Second held message",
      attachments: [],
      references: [],
    };

    useChatComposerStore
      .getState()
      .addPendingFlexibleMessage("thread:thread-a", firstMessage);
    useChatComposerStore
      .getState()
      .addPendingFlexibleMessage("thread:thread-a", secondMessage);
    useChatComposerStore
      .getState()
      .removePendingFlexibleMessage("thread:thread-a", firstMessage.id);

    expect(useChatComposerStore.getState().pendingFlexibleMessagesBySession).toEqual({
      "thread:thread-a": [secondMessage],
    });
  });

  it("updates the configured link open gesture", () => {
    useChatComposerStore.getState().setLinkOpenGesture("click");

    expect(useChatComposerStore.getState().linkOpenGesture).toBe("click");
  });
});
