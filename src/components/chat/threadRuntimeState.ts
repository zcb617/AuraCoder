import type { Thread } from "../../types";

/**
 * 判断会话是否仍处于首次发送前的空状态。
 *
 * 空会话尚未建立 CLI 会话且没有消息，可以在首次发送前原地切换 CLI；
 * 具体工作区位置和后端消息表校验不属于本地状态预判职责。
 */
export function canChangeUnstartedThreadEngine(
  thread: Pick<Thread, "engineThreadId" | "messageCount"> | null | undefined,
): boolean {
  return Boolean(
    thread &&
      thread.engineThreadId === null &&
      thread.messageCount === 0,
  );
}
