// Ingest — step router for the 6-step Data Ingestion wizard.
// State lives in the ingestion store (survives navigation). This component just
// reads the current step and renders the appropriate sub-component.
import { RefreshCw } from 'lucide-react';
import { useIngestion } from '../../stores/ingestion';
import { Button } from '../../components/ui';
import { cn } from '../../components/ui/cn';
import { PageContainer } from '../../components/layout/PageContainer';
import PickConnection from './PickConnection';
import LoadingSchema from './LoadingSchema';
import Analyzing from './Analyzing';
import SelectTables from './SelectTables';
import Ingesting from './Ingesting';

export default function Ingest() {
  const step        = useIngestion((s) => s.step);
  const resetWizard = useIngestion((s) => s.resetWizard);

  // Show the "Start over" button from select-tables onwards.
  const showReset =
    step === 'select-tables' || step === 'ingesting' || step === 'complete';

  // The live-run view is a dashboard — counters, progress bars and a streaming
  // log — not a form, so it gets the board width and stretches to the full height
  // of the shell instead of sitting in a narrow column with dead space beneath it.
  // The earlier steps stay capped: they are forms and a table list, which read
  // worse when a wide monitor pulls their labels and values metres apart.
  // Height only takes over at md+ — on a phone the page keeps flowing and
  // scrolling normally.
  const dashboard = step === 'ingesting' || step === 'complete';

  return (
    <PageContainer
      variant={dashboard ? 'board' : 'flow'}
      className={cn(dashboard && 'md:flex md:h-full md:min-h-0 md:flex-col')}
    >
    <div className={cn('flex flex-col gap-5', dashboard && 'md:min-h-0 md:flex-1')}>

      {/* ── Page header ── */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-fg">Data Ingestion</h1>
          <p className="text-sm text-fg-muted mt-0.5">
            Analyse a source database, select tables, and index records into the local store.
          </p>
        </div>
        {showReset && (
          <div className="sm:shrink-0">
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<RefreshCw size={14} />}
              onClick={resetWizard}
              className="w-full sm:w-auto"
            >
              Start over
            </Button>
          </div>
        )}
      </div>

      {/* ── Step content ── */}
      {step === 'pick-connection'  && <PickConnection />}
      {step === 'loading-schema'   && <LoadingSchema />}
      {step === 'analyzing'        && <Analyzing />}
      {step === 'select-tables'    && <SelectTables />}
      {(step === 'ingesting' || step === 'complete') && <Ingesting />}

    </div>
    </PageContainer>
  );
}
