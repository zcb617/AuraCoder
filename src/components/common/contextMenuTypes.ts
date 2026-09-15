import type { ReactNode } from "react";

/**
 * 右键菜单项被选中时执行的业务动作，支持同步和异步处理。
 */
export type ContextMenuSelectHandler = () => void | Promise<void>;

/**
 * 右键菜单定位时使用的触发点矩形，描述菜单应从哪个屏幕位置展开。
 */
export interface ContextMenuTriggerRect {
  /** 触发点在视口中的顶部坐标。 */
  top: number;
  /** 触发点在视口中的底部坐标。 */
  bottom: number;
  /** 触发点在视口中的右侧坐标。 */
  right: number;
}

/**
 * 通用右键菜单的 JSON 配置，包含菜单项、触发位置和可选的外观参数。
 */
export interface ContextMenuConfig {
  /** 当前菜单需要渲染的菜单项及其子菜单配置。 */
  items: ContextMenuItem[];
  /** 菜单骨架关闭时通知调用方更新显示状态的业务事件。 */
  onClose: () => void;
  /** 触发菜单的屏幕横坐标。 */
  x: number;
  /** 触发菜单的屏幕纵坐标。 */
  y: number;
  /** 菜单首次定位时使用的最小宽度。 */
  minWidth?: number;
  /** 菜单首次定位时使用的预估高度。 */
  estimatedHeight?: number;
  /** 菜单根节点追加的业务样式类名。 */
  className?: string;
  /** 菜单项追加的业务样式类名。 */
  itemClassName?: string;
  /** 可选的完整触发点矩形，用于保留特定调用方的定位语义。 */
  triggerRect?: ContextMenuTriggerRect;
}

/**
 * 通用右键菜单项配置，描述菜单项的展示内容、交互动作和嵌套子菜单。
 */
export interface ContextMenuItem {
  /** 菜单项在同级菜单中的稳定唯一标识。 */
  id: string;
  /** 菜单项显示的业务文案。 */
  label: string;
  /** 菜单项左侧显示的图标节点。 */
  icon?: ReactNode;
  /** 是否使用危险操作的视觉样式。 */
  danger?: boolean;
  /** 是否禁止用户触发当前菜单项。 */
  disabled?: boolean;
  /** 当前菜单项被点击后执行的业务事件。 */
  onSelect?: ContextMenuSelectHandler;
  /** 当前菜单项展开时显示的嵌套子菜单项。 */
  children?: ContextMenuItem[];
  /** 是否处于异步加载状态，加载状态下菜单项不可点击。 */
  loading?: boolean;
  /** 是否将当前配置渲染为菜单分隔线。 */
  divider?: boolean;
}

/**
 * 通用右键菜单组件的渲染参数，负责把配置菜单挂载到页面并通知调用方关闭。
 */
export interface ContextMenuProps {
  /** 右键菜单的完整 JSON 配置。 */
  config: ContextMenuConfig;
}
