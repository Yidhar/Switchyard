import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';

export interface ContextMenuItem {
  id: string;
  label?: string;
  shortcut?: string;
  disabled?: boolean;
  danger?: boolean;
  separator?: boolean;
  onSelect?: () => void | Promise<void>;
}

interface ContextMenuProps {
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
}

/// Lightweight VS Code-like context menu used by Explorer and future
/// workbench surfaces. It is fixed-position and clamps itself inside the
/// viewport after first layout, so right-clicking near the bottom/right edge
/// does not push menu items off-screen.
export const ContextMenu: React.FC<ContextMenuProps> = ({
  x,
  y,
  items,
  onClose,
}) => {
  const ref = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState({ x, y });

  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const rect = element.getBoundingClientRect();
    const nextX = Math.min(x, window.innerWidth - rect.width - 8);
    const nextY = Math.min(y, window.innerHeight - rect.height - 8);
    setPos({
      x: Math.max(8, nextX),
      y: Math.max(8, nextY),
    });
  }, [x, y, items]);

  useEffect(() => {
    const handlePointerDown = (event: MouseEvent) => {
      if (!ref.current?.contains(event.target as Node)) {
        onClose();
      }
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        onClose();
      }
    };
    window.addEventListener('mousedown', handlePointerDown, true);
    window.addEventListener('keydown', handleKeyDown);
    window.addEventListener('resize', onClose);
    window.addEventListener('scroll', onClose, true);
    return () => {
      window.removeEventListener('mousedown', handlePointerDown, true);
      window.removeEventListener('keydown', handleKeyDown);
      window.removeEventListener('resize', onClose);
      window.removeEventListener('scroll', onClose, true);
    };
  }, [onClose]);

  const menu = (
    <div
      ref={ref}
      className="switchyard-context-menu"
      style={{ left: pos.x, top: pos.y }}
      onContextMenu={(event) => event.preventDefault()}
    >
      {items.map((item, index) =>
        item.separator ? (
          <div
            key={`${item.id}-${index}`}
            className="switchyard-context-menu-separator"
          />
        ) : (
          <button
            key={item.id}
            type="button"
            disabled={item.disabled}
            className={`switchyard-context-menu-item ${item.danger ? 'is-danger' : ''}`}
            onClick={() => {
              if (item.disabled) return;
              onClose();
              void item.onSelect?.();
            }}
          >
            <span>{item.label}</span>
            {item.shortcut && (
              <span className="switchyard-context-menu-shortcut">
                {item.shortcut}
              </span>
            )}
          </button>
        ),
      )}
    </div>
  );

  // Workbench panes use `contain: paint`/`overflow: hidden` for resize and
  // long-history performance. Rendering the menu inside a pane makes it get
  // clipped at column boundaries and lose z-order to neighboring panes. Portal
  // it to <body> so fixed positioning and z-index are truly viewport-level.
  return createPortal(menu, document.body);
};

export default ContextMenu;
