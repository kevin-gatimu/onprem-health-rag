// Ingest — step router for the 6-step Data Ingestion wizard.
// State lives in the ingestion store (survives navigation). This component just
// reads the current step and renders the appropriate sub-component.
import { RefreshCw } from 'lucide-react';
import { useIngestion } from '../../stores/ingestion';
import { Button } from '../../components/ui';
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

  return (
    <PageContainer variant="flow">
    <div className="flex flex-col gap-5">

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
