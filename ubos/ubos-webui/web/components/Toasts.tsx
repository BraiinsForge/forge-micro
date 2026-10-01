import { useToasts, dismiss } from '../lib/toast';

// Notification stack under the right end of the top bar. Reads the
// shared toast store and renders one card per toast: a colored left
// border, the success/error icon, a header, optional detail, and a
// close button.
export default function Toasts() {
  const { list } = useToasts();
  if (!list.length) return null;
  return (
    <div class="toasts" role="status" aria-live="polite">
      {list.map((t) => (
        <div key={t.id} class={'toast toast-' + t.kind}>
          <svg class="toastIcon" aria-hidden="true">
            <use href={t.kind === 'success' ? '#sok' : '#serr'} />
          </svg>
          <div class="toastBody">
            <div class="toastTitle">{t.title}</div>
            {t.text && <div class="toastText">{t.text}</div>}
          </div>
          <button type="button" class="toastClose" aria-label="Close" onClick={() => dismiss(t.id)}>
            ×
          </button>
        </div>
      ))}
    </div>
  );
}
