// Dashboard — Stage 2 of the mobile-first UI rebuild.
// Mobile-first: designed at 360 px, then md:/lg:/xl: breakpoints scale up.
// All role-conditional content flows through useEffectiveRole() so the admin
// "Preview as" toggle works automatically everywhere on the page.
import type { ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
  FileText, Database, Activity, Bell, Clock, MessageSquare,
  BarChart3, FolderOpen, Download, ShieldCheck, Inbox,
} from 'lucide-react';
import { useEffectiveRole } from '../../hooks/useEffectiveRole';
import { useSession } from '../../stores/session';
import { useUi } from '../../stores/ui';
import { getStats } from '../../lib/bridge';
import type { DashboardStats } from '../../lib/bridge';
import { canAccess } from '../../lib/permissions';
import type { Role, Route } from '../../lib/types';
import { Card, StatCard, EmptyState, Select } from '../../components/ui';
import { PageContainer } from '../../components/layout/PageContainer';

// ── Types ─────────────────────────────────────────────────────────────────────

interface StatDef {
  label: string;
  icon: ReactNode;
  getValue: (s: DashboardStats | undefined, loading: boolean) => string;
  /** Drives the StatCard's accentColor prop — e.g. 'text-danger' when alerts > 0. */
  accentColor?: (s: DashboardStats | undefined) => string | undefined;
}

interface ActionDef {
  icon: ReactNode;
  title: string;
  subtitle: string;
  route: Route;
}

// ── Config ────────────────────────────────────────────────────────────────────

const ROLE_SUBTITLE: Record<Role, string> = {
  admin:   'Full system overview — connections, ingestion, and user management.',
  doctor:  'Your clinical AI workspace — chat, records, and disease insights.',
  nurse:   'Patient data at a glance, and real-time outbreak monitoring.',
  analyst: 'Health analytics, trends, and population data.',
};

/** All possible stat cards; roles select subsets by index below. */
const ALL_STAT_DEFS: StatDef[] = [
  /* 0 */ {
    label: 'Total Records',
    icon: <FileText size={16} />,
    getValue: (s, l) => l || !s ? '—' : s.total_records.toLocaleString(),
  },
  /* 1 */ {
    label: 'Tables Indexed',
    icon: <Database size={16} />,
    getValue: (s, l) => l || !s ? '—' : s.total_tables.toLocaleString(),
  },
  /* 2 */ {
    label: 'Active Connections',
    icon: <Activity size={16} />,
    getValue: (s, l) => l || !s ? '—' : String(s.active_connections),
  },
  /* 3 */ {
    label: 'Pending Alerts',
    icon: <Bell size={16} />,
    getValue: (s, l) => l || !s ? '—' : String(s.pending_alerts),
    // Accent to text-danger when there are alerts; drives urgency without inline color.
    accentColor: (s) => s && s.pending_alerts > 0 ? 'text-danger' : undefined,
  },
  /* 4 */ {
    label: 'Last Ingest',
    icon: <Clock size={16} />,
    getValue: (s, l) => l || !s ? '—' : fmtDate(s.last_ingest_at),
  },
  /* 5 */ {
    label: 'AI Engine',
    icon: <MessageSquare size={16} />,
    // Always "Foundry Local" — not a numeric value, shown immediately.
    getValue: (_s, _loading) => 'Foundry Local',
  },
];

/** Indices into ALL_STAT_DEFS for each role's stat grid. */
const ROLE_STAT_INDICES: Record<Role, number[]> = {
  admin:   [0, 1, 2, 3, 4, 5],
  doctor:  [0, 3, 4, 5],
  nurse:   [0, 3],
  analyst: [0, 1, 3, 4, 5],
};

/** Quick-action lists per role; filtered through canAccess() before rendering. */
const ROLE_ACTIONS: Record<Role, ActionDef[]> = {
  admin: [
    { icon: <Database size={18} />,     title: 'Connect a database', subtitle: 'MySQL, PostgreSQL, MSSQL',               route: '/connections' },
    { icon: <Download size={18} />,     title: 'Ingest data',        subtitle: 'Analyse schema and start indexing',      route: '/ingest' },
    { icon: <MessageSquare size={18} />, title: 'Ask the AI',        subtitle: 'Clinical RAG chat — Foundry Local',      route: '/agents' },
    { icon: <ShieldCheck size={18} />,  title: 'Manage users',       subtitle: 'Create, edit and assign roles',          route: '/admin' },
  ],
  doctor: [
    { icon: <MessageSquare size={18} />, title: 'AI Clinical Chat',        subtitle: 'Differential diagnosis powered by Foundry Local', route: '/agents' },
    { icon: <FolderOpen size={18} />,    title: 'Browse Patient Records',  subtitle: 'Explore indexed clinical data',                   route: '/data' },
    { icon: <BarChart3 size={18} />,     title: 'Disease Trends',          subtitle: 'Analytics & population health',                   route: '/analytics' },
    { icon: <Bell size={18} />,          title: 'Outbreak Alerts',         subtitle: 'Monitor disease patterns in real time',            route: '/alerts' },
  ],
  nurse: [
    { icon: <MessageSquare size={18} />, title: 'AI Agents',         subtitle: 'Clinical support & quick reference',     route: '/agents' },
    { icon: <FolderOpen size={18} />,    title: 'Browse Records',    subtitle: 'View indexed patient data',              route: '/data' },
    // /alerts is admin/doctor/analyst only — canAccess() filters this out for nurse.
    { icon: <Bell size={18} />,          title: 'Outbreak Alerts',   subtitle: 'Monitor active disease alerts',          route: '/alerts' },
  ],
  analyst: [
    { icon: <BarChart3 size={18} />,     title: 'Analytics',         subtitle: 'Disease trends & population health',     route: '/analytics' },
    { icon: <MessageSquare size={18} />, title: 'AI Agents',         subtitle: 'Data analysis with Foundry Local',       route: '/agents' },
    { icon: <FolderOpen size={18} />,    title: 'Browse Records',    subtitle: 'Explore indexed health data',            route: '/data' },
    { icon: <Bell size={18} />,          title: 'Outbreak Alerts',   subtitle: 'Monitor disease patterns',               route: '/alerts' },
  ],
};

// ── Helpers ───────────────────────────────────────────────────────────────────

function greetingPrefix(): string {
  const h = new Date().getHours();
  if (h < 12) return 'Good morning';
  if (h < 17) return 'Good afternoon';
  return 'Good evening';
}

function fmtDate(iso: string | null): string {
  if (!iso) return 'Never';
  return new Date(iso).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

// ── ActionRow sub-component ───────────────────────────────────────────────────

interface ActionRowProps {
  icon: ReactNode;
  title: string;
  subtitle: string;
  onClick: () => void;
}

function ActionRow({ icon, title, subtitle, onClick }: ActionRowProps) {
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onClick}
      onKeyDown={(e) => (e.key === 'Enter' || e.key === ' ') && onClick()}
      // min-h-[44px] satisfies the ≥ 44 px tap-target requirement on mobile.
      className="flex items-center gap-3 min-h-[44px] py-2 px-2 -mx-2 rounded-md
                 cursor-pointer select-none
                 hover:bg-base active:opacity-70
                 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40
                 transition-colors"
    >
      <span className="text-accent flex-shrink-0">{icon}</span>
      <div className="flex flex-col min-w-0">
        <span className="text-sm font-medium text-fg">{title}</span>
        <span className="text-xs text-fg-muted">{subtitle}</span>
      </div>
    </div>
  );
}

// ── Dashboard ─────────────────────────────────────────────────────────────────

export default function Dashboard() {
  // effectiveRole drives all role-conditional rendering; useEffectiveRole() handles
  // the admin preview-as logic so every piece of UI reflects the previewed role.
  const effectiveRole = useEffectiveRole();
  // realRole is needed only to gate visibility of the admin preview select itself.
  const realRole = useSession((s) => s.user?.role ?? null);
  const userName  = useSession((s) => s.user?.name);
  const previewRole    = useUi((s) => s.previewRole);
  const setPreviewRole = useUi((s) => s.setPreviewRole);
  const navigate       = useUi((s) => s.navigate);

  const { data: stats, isLoading } = useQuery({
    queryKey: ['stats'],
    queryFn: getStats,
    staleTime: 30_000,
  });

  const firstName = userName?.split(' ')[0];
  const greeting  = firstName
    ? `${greetingPrefix()}, ${firstName}.`
    : `${greetingPrefix()}.`;
  const subtitle = effectiveRole ? ROLE_SUBTITLE[effectiveRole] : 'Welcome to Health Records Ingest.';

  // Build stat defs from the role's index set.
  const statIndices = effectiveRole ? ROLE_STAT_INDICES[effectiveRole] : [0];

  // Quick actions filtered through the permission matrix; nurse's /alerts entry
  // is dropped here automatically via canAccess().
  const rawActions = effectiveRole ? ROLE_ACTIONS[effectiveRole] : [];
  const actions    = rawActions.filter((a) => canAccess(a.route, effectiveRole));

  const isRunning = stats?.llm_status === 'running';

  return (
    // PageContainer (board) fills available width; AppShell/main already supplies
    // responsive padding so we only need the inner flex column here.
    <PageContainer variant="board">
    <div className="flex flex-col gap-5">

      {/* ── Header ─────────────────────────────────────────────────────────── */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-fg">{greeting}</h1>
          <p className="text-sm text-fg-muted mt-0.5">{subtitle}</p>
        </div>

        {/* Admin-only: "Preview as" select — stacks below heading on narrow screens. */}
        {realRole === 'admin' && (
          <div className="shrink-0 sm:w-52">
            <Select
              label="Preview as"
              value={previewRole ?? ''}
              onChange={(e) => {
                const val = e.target.value;
                setPreviewRole(val ? (val as Role) : null);
              }}
              options={[
                { value: '',        label: '— My view (admin) —' },
                { value: 'doctor',  label: 'Doctor' },
                { value: 'nurse',   label: 'Nurse' },
                { value: 'analyst', label: 'Analyst' },
              ]}
            />
          </div>
        )}
      </div>

      {/* ── Foundry status banner — shown for all roles ─────────────────────── */}
      <div
        className={[
          'flex items-center gap-2.5 rounded-lg border px-4 py-3 text-sm',
          isRunning
            ? 'bg-success-subtle border-success/20 text-success'
            : 'bg-warning-subtle border-warning/20 text-warning',
        ].join(' ')}
      >
        <span
          aria-hidden="true"
          className={[
            'w-2 h-2 rounded-full flex-shrink-0',
            isRunning ? 'bg-success' : 'bg-warning',
          ].join(' ')}
        />
        {isRunning
          ? 'Foundry Local is running — chat and embeddings available.'
          : 'Foundry Local is not running. Start it from Settings, then download a chat model on the Models page.'}
      </div>

      {/* ── Stat grid ──────────────────────────────────────────────────────── */}
      <div className="grid grid-cols-2 gap-3 md:gap-4 lg:grid-cols-3 xl:grid-cols-4 3xl:grid-cols-6">
        {statIndices.map((i) => {
          const def = ALL_STAT_DEFS[i];
          return (
            <StatCard
              key={def.label}
              label={def.label}
              value={def.getValue(stats, isLoading)}
              icon={def.icon}
              accentColor={def.accentColor?.(stats)}
            />
          );
        })}
      </div>

      {/* ── Two-column section: quick actions + outbreak alerts ─────────────── */}
      <div className="grid gap-4 lg:grid-cols-2">

        {/* Quick actions */}
        <Card title="Quick Actions">
          <div className="flex flex-col">
            {actions.map((a) => (
              <ActionRow
                key={a.route}
                icon={a.icon}
                title={a.title}
                subtitle={a.subtitle}
                onClick={() => navigate(a.route)}
              />
            ))}
          </div>
        </Card>

        {/* Outbreak alerts — no backend yet; always empty */}
        <Card title="Outbreak Alerts">
          <EmptyState
            icon={<Inbox size={32} />}
            title="No active alerts"
            description="Outbreak alerts will appear here when disease patterns are detected."
          />
        </Card>

      </div>
    </div>
    </PageContainer>
  );
}
