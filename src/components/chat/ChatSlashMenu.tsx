import { Fragment, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { CliSlashCommand } from "../../cli-tools/contracts/slash-command";

export type SlashCommand = CliSlashCommand;

interface ChatSlashMenuProps {
  visible: boolean;
  commands: SlashCommand[];
  anchorRef: React.RefObject<HTMLElement | null>;
  activeIndex: number;
  onSelect: (commandId: string) => void;
  onDismiss: () => void;
  onActiveChange: (index: number) => void;
}

export function ChatSlashMenu({
  visible,
  commands,
  anchorRef,
  activeIndex,
  onSelect,
  onDismiss,
  onActiveChange,
}: ChatSlashMenuProps) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ bottom: 0, left: 0, width: 0 });

  useLayoutEffect(() => {
    if (!visible || !anchorRef.current) return;
    const anchor = anchorRef.current;
    const updatePosition = () => {
      const rect = anchor.getBoundingClientRect();
      setPos({
        bottom: window.innerHeight - rect.top + 6,
        left: rect.left,
        width: rect.width,
      });
    };

    updatePosition();
    const resizeObserver =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(updatePosition);
    resizeObserver?.observe(anchor);
    window.addEventListener("resize", updatePosition);

    return () => {
      resizeObserver?.disconnect();
      window.removeEventListener("resize", updatePosition);
    };
  }, [visible, anchorRef]);

  // Close on outside click
  useEffect(() => {
    if (!visible) return;

    function onPointerDown(e: PointerEvent) {
      if (menuRef.current?.contains(e.target as Node)) return;
      onDismiss();
    }

    window.addEventListener("pointerdown", onPointerDown);
    return () => window.removeEventListener("pointerdown", onPointerDown);
  }, [visible, onDismiss]);

  // Scroll active item into view
  useEffect(() => {
    if (!visible) return;
    const activeEl = menuRef.current?.querySelector(
      `[data-slash-index="${activeIndex}"]`,
    );
    activeEl?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, visible]);

  if (!visible || commands.length === 0) return null;

  return createPortal(
    <div
      ref={menuRef}
      className="slash-menu"
      style={{
        position: "fixed",
        zIndex: 1400,
        bottom: pos.bottom,
        left: pos.left,
        width: pos.width,
      }}
    >
      {commands.map((cmd, i) => {
        const Icon = cmd.icon;
        const isActive = i === activeIndex;
        const previousGroup = i > 0 ? commands[i - 1]?.group : undefined;
        const showGroupHeading = Boolean(cmd.group && cmd.group !== previousGroup);
        return (
          <Fragment key={cmd.id}>
            {showGroupHeading && (
              <div className="slash-menu-group">{cmd.group}</div>
            )}
            <button
              type="button"
              data-slash-index={i}
              className={`slash-menu-item${isActive ? " slash-menu-item-active" : ""}${cmd.disabled ? " slash-menu-item-disabled" : ""}`}
              onPointerEnter={() => onActiveChange(i)}
              onClick={() => {
                if (!cmd.disabled) onSelect(cmd.id);
              }}
              disabled={cmd.disabled}
            >
              <span className="slash-menu-item-icon">
                <Icon size={14} />
              </span>
              <span className="slash-menu-item-text">
                <span className="slash-menu-item-name">
                  {cmd.name[0].toUpperCase() + cmd.name.slice(1)}
                </span>
                <span className="slash-menu-item-desc">{cmd.description}</span>
              </span>
              {/* CLI 适配器已经在构建菜单时隔离项目，不再以徽章混排其他 CLI 命令。 */}
            </button>
          </Fragment>
        );
      })}
    </div>,
    document.body,
  );
}
