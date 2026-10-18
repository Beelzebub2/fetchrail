import { useEffect, useRef, type ReactNode } from "react";

export function Modal({ label, busy = false, onClose, children }: { label: string; busy?: boolean; onClose: () => void; children: ReactNode }) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    const element = dialog.current!;
    element.showModal();
    return () => { element.close(); if (opener?.isConnected) opener.focus(); };
  }, []);
  return <dialog ref={dialog} className="modal-backdrop" aria-labelledby={label}
    onCancel={event => { event.preventDefault(); if (!busy) onClose(); }}
    onMouseDown={event => { if (event.target === event.currentTarget && !busy) onClose(); }}>
    {children}
  </dialog>;
}
