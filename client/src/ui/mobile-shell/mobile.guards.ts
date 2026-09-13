/**
 * Form-factor route guards (WP-M2 / F10-8).
 *
 * The phone shell lives under `/m/**`; the desktop shell keeps the flat
 * routes. Each screen has a twin on the other side, so a deep link opened on
 * the "wrong" form factor is redirected instead of rendering the wrong shell:
 *
 * | desktop        | mobile             |
 * |----------------|--------------------|
 * | `/`            | `/m/chat`          |
 * | `/chat/:id`    | `/m/chat/:id`      |
 * | `/settings`    | `/m/more/settings` |
 * | `/stats`       | `/m/more/stats`    |
 * | `/about`       | `/m/more/about`    |
 * | (desktop-only) | `/m/more`          |
 * | `/`            | `/m/remote`, `/m/agents`, `/m/changes`, `/m/more` |
 * | `/chat/:id`    | `/m/remote/:id`    |
 *
 * `redirectToMobile` is a `CanActivateFn` on every desktop route;
 * `redirectToDesktop` a `CanMatchFn` on the lazy `m` route so the mobile
 * chunk is never even downloaded on a desktop.
 */

import { inject } from '@angular/core';
import {
  ActivatedRouteSnapshot,
  CanActivateFn,
  CanMatchFn,
  Params,
  Router,
  UrlSegment,
} from '@angular/router';

import { FormFactor } from '../../core/form-factor';

/** Path of the `/m/**` twin of a desktop path (both without query/hash). */
export function mobileTwin(path: string): string {
  const parts = segments(path);
  const [head, ...rest] = parts;
  switch (head) {
    case undefined:
    case '':
    case 'connect':
      return '/m/chat';
    case 'chat':
      return rest[0] ? `/m/chat/${rest[0]}` : '/m/chat';
    case 'settings':
    case 'config':
      return '/m/more/settings';
    case 'stats':
      return '/m/more/stats';
    case 'about':
      return '/m/more/about';
    default:
      // terminal / explorer / debug: desktop-only screens.
      return '/m/more';
  }
}

/** Path of the desktop twin of a `/m/**` path. */
export function desktopTwin(path: string): string {
  const parts = segments(path);
  if (parts[0] !== 'm') {
    return path.startsWith('/') ? path : `/${path}`;
  }
  const [, tab, ...rest] = parts;
  switch (tab) {
    case 'chat':
    case 'remote':
      return rest[0] ? `/chat/${rest[0]}` : '/';
    case 'more':
      switch (rest[0]) {
        case 'settings':
          return '/settings';
        case 'stats':
          return '/stats';
        case 'about':
          return '/about';
        default:
          return '/';
      }
    default:
      return '/';
  }
}

function segments(path: string): string[] {
  return path
    .split('?')[0]
    .split('#')[0]
    .split('/')
    .filter((s) => s.length > 0);
}

/** Desktop routes: on a phone, go to the `/m/**` twin (query params kept). */
export const redirectToMobile: CanActivateFn = (route: ActivatedRouteSnapshot) => {
  const formFactor = inject(FormFactor);
  if (!formFactor.isMobile()) {
    return true;
  }
  const router = inject(Router);
  const path = '/' + route.url.map((s) => s.path).join('/');
  return router.createUrlTree([mobileTwin(path)], { queryParams: route.queryParams });
};

/** The lazy `m` route: on a desktop, go back to the flat twin instead. */
export const redirectToDesktop: CanMatchFn = (_route, urlSegments: UrlSegment[]) => {
  const formFactor = inject(FormFactor);
  if (formFactor.isMobile()) {
    return true;
  }
  const router = inject(Router);
  const path = '/' + urlSegments.map((s) => s.path).join('/');
  const queryParams: Params = router.getCurrentNavigation()?.extractedUrl.queryParams ?? {};
  return router.createUrlTree([desktopTwin(path)], { queryParams });
};
