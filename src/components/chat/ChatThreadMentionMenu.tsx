import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { MessageSquare } from "lucide-react";
import type { Thread } from "../../types";

interface ChatThreadMentionMenuProps {
  /** 是否显示菜单。 */
  visible: boolean;
  /** 当前项目中符合查询条件的候选会话。 */
  threads: Thread[];
  /** 用于定位菜单的输入框。 */
  anchorRef: React.RefObject<HTMLElement | null>;
  /** 当前键盘活动项。 */
  activeIndex: number;
  /** 选择候选会话。 */
  onSelect: (thread: Thread) => void;
  /** 关闭菜单。 */
  onDismiss: () => void;
  /** 更新活动项。 */
  onActiveChange: (index: number) => void;
}

/** 输入框中的 @AuraCoder 会话候选菜单。 */
export function ChatThreadMentionMenu({
  visible,
  threads,
  anchorRef,
  activeIndex,
  onSelect,
  onDismiss,
  onActiveChange,
}: ChatThreadMentionMenuProps) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ bottom: 0, left: 0, width: 0 });

  useLayoutEffect(() => {
    if (!visible || !anchorRef.current) return;
    const anchor = anchorRef.current;
    const updatePosition = () => {
      const rect = anchor.getBoundingClientRect();
      setPos({ bottom: window.innerHeight - rect.top + 6, left: rect.left, width: rect.width });
    };
    updatePosition();
    window.addEventListener("resize", updatePosition);
    return () => window.removeEventListener("resize", updatePosition);
  }, [anchorRef, visible]);

  useEffect(() => {
    if (!visible) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Element) || !menuRef.current?.contains(target)) onDismiss();
    };
    window.addEventListener("pointerdown", onPointerDown);
    return () => window.removeEventListener("pointerdown", onPointerDown);
  }, [onDismiss, visible]);

  useEffect(() => {
    if (!visible) return;
    menuRef.current
      ?.querySelector(`[data-thread-mention-index="${activeIndex}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, visible]);

  if (!visible || threads.length === 0) return null;
  return createPortal(
    <div
      ref={menuRef}
      className="slash-menu"
      style={{ position: "fixed", zIndex: 1400, bottom: pos.bottom, left: pos.left, width: pos.width }}
    >
      {threads.map((thread, index) => {
        const title = thread.title.trim() || "未命名会话";
        return (
          <button
            key={thread.id}
            type="button"
            data-thread-mention-index={index}
            className={`slash-menu-item${index === activeIndex ? " slash-menu-item-active" : ""}`}
            onPointerEnter={() => onActiveChange(index)}
            onClick={() => onSelect(thread)}
          >
            <span className="slash-menu-item-icon"><MessageSquare size={14} /></span>
            <span className="slash-menu-item-text">
              <span className="slash-menu-item-name">{title}</span>
              <span className="slash-menu-item-desc">{thread.engineId}</span>
            </span>
          </button>
        );
      })}
    </div>,
    document.body,
  );
}
