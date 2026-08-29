import { BarChart3 } from 'lucide-react';
import { StubPage } from '../../components/ui';

// Deliberate stub: Analytics has no backend and no reference screens — it is a
// planned capability, not a screen still being ported. Honest copy instead of
// the generic "being rebuilt" fallback that routes.tsx renders for unwired routes.
export default function Analytics() {
  return (
    <StubPage
      title="Analytics"
      icon={<BarChart3 size={32} />}
      description="Cohort trends, ingestion throughput, and retrieval-quality dashboards will live here. The structured aggregation that feeds them already exists via the AI Agents screen — the dashboards on top are the planned work."
    />
  );
}
