import { useEffect, useRef } from 'react';

/**
 * 全应用统一的模态弹层容器。
 *
 * 此前每个功能弹窗各自手写 `fixed inset-0 flex items-center justify-center`
 * + 遮罩 + z-index，导致 z 值混乱（30/50/100/200/300/5000/6000/7000/9000
 * 混用）且嵌套弹层时 Esc/遮罩点击行为不一致。统一为：
 *
 * - z 层级取 Z_TIERS 的具名档（数值即历史上各层在用的值，语义见注释）；
 * - Esc 只关闭栈顶弹层（模块级栈，嵌套场景不会误关底层）；
 * - 遮罩点击用 mousedown + 目标自检，避免面板内拖拽选中文字松手时误关；
 * - 面板默认夹在视口内（max-h 留出边距），超长内容自行在内部滚动。
 */

export const Z_TIERS = {
  /** 图片放大预览（在普通弹层之下，沿用历史行为）。 */
  lightbox: 100,
  /** 普通功能弹窗。 */
  modal: 200,
  /** 从另一弹窗之上再开的弹窗（如锁密码、二级选择器）。 */
  modalStacked: 300,
  /** 桌宠浮动面板。 */
  petPanel: 4000,
  /** 标题栏下拉菜单。 */
  titlebarMenu: 5000,
  /** 全局工作区弹窗（library AI 系列、导出等，需在桌宠/标题栏之上）。 */
  workspaceModal: 6000,
  /** 应用级 alert/confirm/prompt（ui/Dialog），必须压过一切。 */
  appDialog: 7000,
  /** 引导向导。 */
  onboarding: 9000,
  /** 启动加载全屏遮罩。 */
  bootBlocker: 9999,
} as const;

export type ZTier = keyof typeof Z_TIERS;

const modalStack: number[] = [];
let nextModalId = 1;

function isTopModal(id: number): boolean {
  return modalStack[modalStack.length - 1] === id;
}

export interface ModalProps {
  /** 提供时启用关闭交互（Esc + 遮罩点击，可用开关分别关闭）。 */
  onClose?: () => void;
  children: React.ReactNode;
  /** 面板类名：宽度、背景、边框、圆角等。 */
  className?: string;
  /** 遮罩类名，默认 bg-black/50。 */
  backdropClassName?: string;
  zTier?: ZTier;
  /** center（默认）或 top（命令面板式，配合 topOffset）。 */
  position?: 'center' | 'top';
  topOffset?: string;
  closeOnEscape?: boolean;
  closeOnBackdrop?: boolean;
}

export function Modal({
  onClose,
  children,
  className = '',
  backdropClassName = 'bg-black/50',
  zTier = 'modal',
  position = 'center',
  topOffset,
  closeOnEscape = true,
  closeOnBackdrop = true,
}: ModalProps) {
  const idRef = useRef(0);
  if (idRef.current === 0) idRef.current = nextModalId++;
  const id = idRef.current;

  useEffect(() => {
    modalStack.push(id);
    return () => {
      const i = modalStack.indexOf(id);
      if (i >= 0) modalStack.splice(i, 1);
    };
  }, [id]);

  useEffect(() => {
    if (!onClose || !closeOnEscape) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && isTopModal(id)) {
        e.preventDefault();
        onClose();
      }
    };
    // 用冒泡阶段：弹层内输入框的 onKeyDown（React 合成事件）先执行，其
    // stopPropagation（如「Esc 取消重命名」）能阻止整个弹层被误关。
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [id, onClose, closeOnEscape]);

  const positionClasses =
    position === 'center' ? 'items-center justify-center' : 'items-start justify-center';

  return (
    <div
      className={`fixed inset-0 flex p-4 ${positionClasses}`}
      style={{ zIndex: Z_TIERS[zTier], paddingTop: position === 'top' ? topOffset : undefined }}
    >
      <div
        className={`absolute inset-0 ${backdropClassName}`}
        onMouseDown={(e) => {
          if (closeOnBackdrop && onClose && e.target === e.currentTarget) onClose();
        }}
      />
      <div className={`relative max-h-full ${className}`}>{children}</div>
    </div>
  );
}
