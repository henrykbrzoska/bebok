/**
 * App shell (WP-SHELL / F1-2): the persistent three-zone desktop layout.
 *
 * `display:flex; height:100vh` - left sidebar at its fixed width, then a main
 * column (`flex:1`) made of the 46px topbar and a content row. Screens are
 * still routes; the `<router-outlet>` lives in the content area, so every
 * existing deep link keeps working while sidebar, topbar, right drawer and
 * command palette stay mounted across navigations.
 */

import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';
import { RouterOutlet } from '@angular/router';

import { CommandPalette } from '../command-palette/command-palette';
import { DirectoryBrowser } from '../directory-browser/directory-browser';
import { ProjectSwitcher } from '../project-switcher/project-switcher';
import { RightDrawer } from '../right-drawer/right-drawer';
import { Sidebar } from '../sidebar/sidebar';
import { Topbar } from '../topbar/topbar';
import { ShellStore } from './shell.store';

@Component({
  selector: 'app-shell',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterOutlet, Sidebar, Topbar, RightDrawer, CommandPalette, DirectoryBrowser, ProjectSwitcher],
  templateUrl: './app-shell.html',
  styleUrl: './app-shell.css',
})
export class AppShell {
  readonly shell = inject(ShellStore);

  /** The right drawer belongs to the Chat screen only. */
  readonly showDrawer = computed(() => this.shell.isChat() && this.shell.rightDrawerOpen());
  readonly density = this.shell.density;
}
