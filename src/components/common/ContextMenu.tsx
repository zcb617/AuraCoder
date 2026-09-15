import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { ChevronRight } from "lucide-react";
import { getActionMenuPosition } from "../git/actionMenuPosition";
import type {
  ContextMenuConfig,
  ContextMenuItem,
  ContextMenuProps,
} from "./contextMenuTypes";

const CONTEXT_MENU_DEFAULT_MIN_WIDTH = 140;
const CONTEXT_MENU_DEFAULT_ITEM_HEIGHT = 32;
const CONTEXT_MENU_DEFAULT_DIVIDER_HEIGHT = 9;
const CONTEXT_MENU_DEFAULT_VERTICAL_PADDING = 8;
const CONTEXT_MENU_SUBMENU_WIDTH = 208;
const CONTEXT_MENU_SUBMENU_GAP = 12;

interface ContextMenuItemViewProps {
  /** 当前需要渲染的菜单项配置。 */
  item: ContextMenuItem;
  /** 统一关闭当前菜单及其所有嵌套子菜单的业务事件。 */
  onClose: () => void;
  /** 当前已经展开的子菜单路径，路径中包含从根菜单到当前菜单的各级标识。 */
  openSubmenuPath: string[];
  /** 更新当前展开子菜单路径的状态事件。 */
  setOpenSubmenuPath: (path: string[]) => void;
  /** 当前菜单项在菜单树中的父级标识路径。 */
  parentPath: string[];
  /** 当前菜单是否需要把子菜单放到父菜单左侧。 */
  submenuOnLeft: boolean;
  /** 当前层菜单项需要追加的业务样式类名。 */
  additionalClassName?: string;
}

/**
 * 根据单个菜单项配置渲染普通项、分隔线或可继续展开的嵌套子菜单。
 */
function ContextMenuItemView({
  item,
  onClose,
  openSubmenuPath,
  setOpenSubmenuPath,
  submenuOnLeft,
  parentPath,
  additionalClassName,
}: ContextMenuItemViewProps): ReactNode {
  if (item.divider) {
    return <div className="git-action-menu-divider" role="separator" />;
  }

  const hasChildren = Boolean(item.children && item.children.length > 0);
  const disabled = Boolean(item.disabled || item.loading);
  const itemClassName = [
    item.danger
      ? "git-action-menu-item git-action-menu-item-danger"
      : "git-action-menu-item",
    additionalClassName,
  ].filter(Boolean).join(" ");
  const itemPath = [...parentPath, item.id];
  const itemIsOpen = itemPath.every((id, index) => openSubmenuPath[index] === id)
    && openSubmenuPath.length >= itemPath.length;

  const handleItemSelect = () => {
    if (disabled) {
      return;
    }
    if (hasChildren) {
      setOpenSubmenuPath(itemIsOpen ? parentPath : itemPath);
      return;
    }
    onClose();
    if (item.onSelect) {
      void item.onSelect();
    }
  };

  if (hasChildren) {
    return (
      <div
        className="chat-file-context-menu-open-with"
        onPointerLeave={() => setOpenSubmenuPath(parentPath)}
      >
        <button
          type="button"
          className={itemClassName}
          disabled={disabled}
          aria-haspopup="menu"
          aria-expanded={itemIsOpen}
          onPointerEnter={() => {
            if (!disabled) {
              setOpenSubmenuPath(itemPath);
            }
          }}
          onClick={handleItemSelect}
        >
          {item.icon}
          {item.label}
          <ChevronRight size={14} />
        </button>
        {itemIsOpen && (
          <div
            className="git-action-menu chat-file-context-menu-submenu"
            role="menu"
            style={submenuOnLeft ? { right: "calc(100% - 3px)" } : undefined}
          >
            {item.children?.map((child) => (
              <ContextMenuItemView
                key={child.id}
                item={child}
                onClose={onClose}
                openSubmenuPath={openSubmenuPath}
                setOpenSubmenuPath={setOpenSubmenuPath}
                submenuOnLeft={submenuOnLeft}
                parentPath={itemPath}
              />
            ))}
          </div>
        )}
      </div>
    );
  }

  return (
    <button
      type="button"
      className={itemClassName}
      disabled={disabled}
      onClick={handleItemSelect}
    >
      {item.icon}
      {item.label}
    </button>
  );
}

/**
 * 渲染通用右键菜单骨架，负责定位、Portal 挂载、子菜单交互和统一关闭行为。
 */
export function ContextMenu({ config }: ContextMenuProps): ReactNode {
  const menuRef = useRef<HTMLDivElement>(null);
  const onClose = config.onClose;
  const [openSubmenuPath, setOpenSubmenuPath] = useState<string[]>([]);
  const triggerRect = config.triggerRect ?? {
    top: config.y,
    bottom: config.y,
    right: config.x,
  };
  const estimatedHeight = config.estimatedHeight ?? config.items.reduce(
    (height, item) => height + (
      item.divider
        ? CONTEXT_MENU_DEFAULT_DIVIDER_HEIGHT
        : CONTEXT_MENU_DEFAULT_ITEM_HEIGHT
    ),
    CONTEXT_MENU_DEFAULT_VERTICAL_PADDING,
  );
  const menuMinWidth = config.minWidth ?? CONTEXT_MENU_DEFAULT_MIN_WIDTH;
  const [position, setPosition] = useState(() => getActionMenuPosition({
    triggerRect,
    menuWidth: menuMinWidth,
    menuHeight: estimatedHeight,
    viewportWidth: window.innerWidth,
    viewportHeight: window.innerHeight,
    horizontalPlacement: "after",
    verticalPlacement: "clamp",
  }));

  useEffect(() => {
    setOpenSubmenuPath([]);
  }, [config.x, config.y]);

  useEffect(() => {
    function closeOnOutsidePointerDown(event: PointerEvent) {
      if (menuRef.current?.contains(event.target as Node)) {
        return;
      }
      onClose();
    }

    function closeOnContextMenu(event: MouseEvent) {
      if (menuRef.current?.contains(event.target as Node)) {
        return;
      }
      onClose();
    }

    function closeOnEscape(event: KeyboardEvent) {
      if (event.key !== "Escape") {
        return;
      }
      event.stopPropagation();
      onClose();
    }

    document.addEventListener("pointerdown", closeOnOutsidePointerDown, true);
    document.addEventListener("contextmenu", closeOnContextMenu, true);
    document.addEventListener("keydown", closeOnEscape, true);
    document.addEventListener("scroll", onClose, true);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointerDown, true);
      document.removeEventListener("contextmenu", closeOnContextMenu, true);
      document.removeEventListener("keydown", closeOnEscape, true);
      document.removeEventListener("scroll", onClose, true);
    };
  }, [onClose]);

  useLayoutEffect(() => {
    if (!menuRef.current) {
      return;
    }

    const nextPosition = getActionMenuPosition({
      triggerRect,
      menuWidth: menuRef.current.offsetWidth,
      menuHeight: menuRef.current.offsetHeight,
      viewportWidth: window.innerWidth,
      viewportHeight: window.innerHeight,
      horizontalPlacement: "after",
      verticalPlacement: "clamp",
    });
    setPosition((current) => (
      current.top === nextPosition.top && current.left === nextPosition.left
        ? current
        : nextPosition
    ));
  }, [config.x, config.y, config.minWidth, config.estimatedHeight, triggerRect]);

  const submenuOnLeft = position.left + menuMinWidth + CONTEXT_MENU_SUBMENU_WIDTH
    + CONTEXT_MENU_SUBMENU_GAP > window.innerWidth;
  const rootClassName = config.className
    ? `git-action-menu ${config.className}`
    : "git-action-menu";
  const menuStyle: CSSProperties = {
    position: "fixed",
    top: position.top,
    left: position.left,
    minWidth: menuMinWidth,
  };

  return createPortal(
    <div ref={menuRef} className={rootClassName} style={menuStyle} role="menu">
      {config.items.map((item) => (
        <ContextMenuItemView
          key={item.id}
          item={item}
          onClose={onClose}
          openSubmenuPath={openSubmenuPath}
          setOpenSubmenuPath={setOpenSubmenuPath}
          submenuOnLeft={submenuOnLeft}
          parentPath={[]}
          additionalClassName={config.itemClassName}
        />
      ))}
    </div>,
    document.body,
  );
}

export type { ContextMenuConfig, ContextMenuItem, ContextMenuProps } from "./contextMenuTypes";
