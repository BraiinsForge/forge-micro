// Tab title shared by the dashboard and the login page: the brand name
// and the board's host, so every tab for one board reads one way from
// sign-in through the running dashboard. The brand the board reports
// replaces the one fixed at build time. The login page is
// framework-free and imports only this function.
export const tabTitle = (host?: string, brand?: string | null): string =>
  `${brand || __BRAND_NAME__} ${host || location.hostname}`;
