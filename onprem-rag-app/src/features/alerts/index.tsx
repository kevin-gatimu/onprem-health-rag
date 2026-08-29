import { Bell } from 'lucide-react';
import { StubPage } from '../../components/ui';

// Deliberate stub: no alert backend is wired yet. Rule- and anomaly-based alerting
// over ingested records is a planned capability, so this renders honest copy rather
// than the generic "being rebuilt" fallback.
export default function Alerts() {
  return (
    <StubPage
      title="Outbreak Alerts"
      icon={<Bell size={32} />}
      description="Rule- and anomaly-based alerts over incoming records (for example, reportable-condition spikes) will surface here. No alert backend is wired yet — this is a planned capability."
    />
  );
}
