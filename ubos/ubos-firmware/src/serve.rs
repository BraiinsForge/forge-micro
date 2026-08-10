// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! The three HTTP listeners every board serves.

/// Declare the listener bring-up every board runs once its stack is up.
///
/// Invoke once at item position in a board crate, then call what it
/// declares from the boot path:
///
/// ```ignore
/// ubos_firmware::serve_all!(config::MACHINE, &config::MINING_INFO, &config::BOUNDS);
///
/// // Once the stack has an address:
/// serve_all(spawner, stack, thermal, manager_handle).await;
/// ```
///
/// The expansion is one `async fn serve_all`, which seeds each
/// listener's credentials, builds its service, binds its port and spawns
/// its worker pool: the REST API on :8080, the Web UI on :80 and the
/// Prometheus metrics on :8081. The three worker-pool shells live inside
/// that function, so a board declares no HTTP task of its own and the
/// pool sizes stay where the rest of the board's shape is, in
/// `$machine.control_board`.
///
/// It also spawns the credential-sync task, so a board that invokes this
/// must invoke [`sync_credentials_task!`](crate::sync_credentials_task)
/// too. That is the same pairing `ubos_http::serve_http_pool!` has with
/// `ubos_http::http_worker_task!`, one level up.
#[macro_export]
macro_rules! serve_all {
    ($machine:expr, $mining_info:expr, $bounds:expr $(,)?) => {
        /// Bind the REST API, Web UI and metrics listeners, and spawn a
        /// worker pool for each.
        ///
        /// Call once, after the stack has an address.
        #[allow(clippy::items_after_statements)]
        async fn serve_all(
            spawner: embassy_executor::Spawner,
            stack: embassy_net::Stack<'static>,
            thermal: ubos_thermal::Handle,
            manager: ubos_manager::Handle,
        ) {
            // Bound to names of their own so what reaches the task
            // attribute is an identifier. The pool sizes are const
            // expressions either way, and the pool footprint they size is
            // still checked at the link.
            const API_POOL: usize = $machine.control_board.api_http_pool;
            const WEBUI_POOL: usize = $machine.control_board.webui_http_pool;
            const METRICS_POOL: usize = $machine.control_board.metrics_http_pool;

            $crate::http_worker_task!(api_worker_task, ubos_api::RestApiService, API_POOL);
            $crate::http_worker_task!(webui_worker_task, ubos_webui::DashboardService, WEBUI_POOL);
            $crate::http_worker_task!(
                metrics_worker_task,
                ubos_metrics::metrics::PrometheusMetricsService,
                METRICS_POOL
            );

            // REST API on :8080. Seed credentials once, then spawn one worker
            // task per pool slot so each has its own static TaskStorage.
            ubos_api::seed_api_credentials().await;
            static API_SERVICE: static_cell::StaticCell<ubos_api::RestApiService> =
                static_cell::StaticCell::new();
            let api_service: &'static ubos_api::RestApiService =
                API_SERVICE.init(ubos_api::RestApiService::new(
                    $machine.control_board.name,
                    $mining_info,
                    $bounds,
                    thermal,
                    manager,
                    ubos_psu::handle(),
                    stack,
                ));
            $crate::serve_http_pool!(
                spawner,
                stack,
                api_worker_task,
                api_service,
                API_POOL,
                ubos_api::consts::Api
            );
            spawner.spawn(::defmt::unwrap!(sync_credentials_task()));

            // Web UI on :80. Seed credentials once; the API listener's
            // sync_credentials_task keeps them current after first boot.
            ubos_webui::seed_webui_credentials().await;
            static WEBUI_SERVICE: ubos_webui::DashboardService = ubos_webui::DashboardService;
            $crate::serve_http_pool!(
                spawner,
                stack,
                webui_worker_task,
                &WEBUI_SERVICE,
                WEBUI_POOL,
                ubos_webui::consts::Http
            );

            // Prometheus metrics on :8081.
            static METRICS_SERVICE: ubos_metrics::metrics::PrometheusMetricsService =
                ubos_metrics::metrics::PrometheusMetricsService;
            $crate::serve_http_pool!(
                spawner,
                stack,
                metrics_worker_task,
                &METRICS_SERVICE,
                METRICS_POOL,
                ubos_metrics::consts::Metrics
            );
        }
    };
}
