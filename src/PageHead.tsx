import type { ReactNode } from "react";
import type { LucideIcon } from "lucide-react";

// The top of every settings section: the section's icon beside its title and one line on what it covers,
// set off from the cards by a rule, with any section-wide actions on the right.
export function PageHead({ id, icon: Icon, title, actions, children }: { id: string; icon: LucideIcon; title: string; actions?: ReactNode; children: ReactNode }) {
  return (
    <div className="page-head section-head rise">
      <span className="page-icon" aria-hidden="true">
        <Icon size={20} />
      </span>
      <div className="page-title">
        <h1 id={id}>{title}</h1>
        <p>{children}</p>
      </div>
      {actions && <div className="page-actions">{actions}</div>}
    </div>
  );
}
