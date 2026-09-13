/**
 * Mobile routes (WP-M2 / F10-8), lazy-loaded under `/m` (see `app.routes.ts`).
 *
 * They render in the root `<router-outlet>`, which on a phone sits inside
 * `MobileShell` (mounted by `App`, exactly like `AppShell` on the desktop).
 * The five bottom tabs map to the first segment; deeper screens (Stats,
 * Settings, About) live under More. Every route carries `data.screen` so
 * `ShellStore.activeScreen` / `currentSessionId` keep working for the stores
 * the existing views rely on (the chat drawer logic, session tabs, ...).
 *
 * Tab contents are placeholders here: WP-M5 fills `chat-tab` and `more-tab`,
 * WP-M6 fills `remote-tab`, `agents-tab` and `changes-tab` - one file each so
 * the two packages never touch the same component.
 */

import { Routes } from '@angular/router';

import { AboutView } from '../../views/about/about';
import { SettingsView } from '../../views/settings/settings';
import { StatsView } from '../../views/stats/stats';
import { AgentsTab } from './tabs/agents-tab';
import { ChangesTab } from './tabs/changes-tab';
import { ChatTab } from './tabs/chat-tab';
import { MoreTab } from './tabs/more-tab';
import { RemoteTab } from './tabs/remote-tab';

export const MOBILE_ROUTES: Routes = [
  { path: '', redirectTo: 'chat', pathMatch: 'full' },
  { path: 'chat', component: ChatTab, data: { screen: 'start', tab: 'chat' } },
  { path: 'chat/:sessionID', component: ChatTab, data: { screen: 'chat', tab: 'chat' } },
  { path: 'remote', component: RemoteTab, data: { screen: 'start', tab: 'remote' } },
  { path: 'remote/:sessionID', component: RemoteTab, data: { screen: 'chat', tab: 'remote' } },
  { path: 'agents', component: AgentsTab, data: { screen: 'start', tab: 'agents' } },
  { path: 'changes', component: ChangesTab, data: { screen: 'start', tab: 'changes' } },
  { path: 'more', component: MoreTab, data: { screen: 'start', tab: 'more' } },
  { path: 'more/stats', component: StatsView, data: { screen: 'stats', tab: 'more' } },
  { path: 'more/settings', component: SettingsView, data: { screen: 'settings', tab: 'more' } },
  { path: 'more/about', component: AboutView, data: { screen: 'about', tab: 'more' } },
  { path: '**', redirectTo: 'chat' },
];
