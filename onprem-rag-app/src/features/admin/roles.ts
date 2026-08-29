// Shared role display maps for the Profile and Admin screens.
// Only the four roles the server issues — there is no `readonly` role.
import type { Role } from '../../lib/types';

export const ROLE_OPTIONS: Role[] = ['admin', 'doctor', 'nurse', 'analyst'];

export const ROLE_LABEL: Record<Role, string> = {
  admin: 'System Administrator',
  doctor: 'Doctor',
  nurse: 'Nurse',
  analyst: 'Data Analyst',
};

/** Maps each role to the Badge variant token used throughout both screens. */
export const ROLE_BADGE: Record<Role, 'error' | 'info' | 'success' | 'warning'> = {
  admin: 'error',
  doctor: 'info',
  nurse: 'success',
  analyst: 'warning',
};
