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

//! The two HTTP listeners every board serves.

/// Declare the listener bring-up every board runs once its stack is up.
///
/// Invoke once at item position in a board crate, then call the
/// `async fn serve_all` it declares from the boot path, once the stack
/// has an address. The expansion seeds each listener's credentials,
/// builds its service, binds its port and spawns its worker pool: the web
/// UI and the REST API together on :80, and the Prometheus metrics on
/// :8081. The two worker-pool shells live inside that function, so a
/// board declares no HTTP task of its own and the pool sizes stay where
/// the rest of the board's shape is, in `$machine.control_board`.
///
/// ```ignore
/// ubos_firmware::serve_all!(
///     config::MACHINE.control_board.http_pool_sizes,
///     &config::MINING_INFO,
///     &config::BOUNDS,
///     config::MAX_METRICS,
///     config::METRICS_RESPONSE_BYTES,
/// );
///
/// // From the boot path, before the stack is up:
/// spawner.spawn(unwrap!(metrics_ingest_task()));
///
/// // Once the stack has an address:
/// serve_all(spawner, stack, handles, board_name).await;
/// ```
///
/// The expansion is one `async fn serve_all`, which seeds each
/// listener's credentials, builds its service, binds its port and spawns
/// its worker pool: the web UI and the REST API together on :80, and the
/// Prometheus metrics on :8081. The two worker-pool shells live inside
/// that function, so a board declares no HTTP task of its own and the
/// pool sizes stay with the board, which passes them as
/// `$http_pool_sizes`.
///
/// The web UI and the REST API reach one pool behind a
/// [`ubos_http::PrefixRouter`]. This crate wires that router rather than
/// holding it: it depends on hardware-coupled crates, so it does not
/// build on the host and carries no test, and routing is the part of the
/// merged listener worth testing.
///
/// It also spawns the credential-sync task, so a board that invokes this
/// must invoke [`sync_credentials_task!`](crate::sync_credentials_task)
/// too.
///
/// The exposition map, its render buffer and the ingestion task are items
/// rather than locals, because a board spawns ingestion before the stack
/// is up and a publisher that pushed earlier would block on a full
/// channel.
#[macro_export]
macro_rules! serve_all {
    (
        $http_pool_sizes:expr,
        $mining_info:expr,
        $bounds:expr,
        $max_metrics:expr,
        $metrics_response_bytes:expr $(,)?
    ) => {
        ubos_metrics::metrics_statics!(METRICS_STATICS, $max_metrics, $metrics_response_bytes);

        /// Fold pushed metrics into the exposition map. Spawn once, from
        /// the boot path, before the stack is up.
        #[embassy_executor::task]
        async fn metrics_ingest_task() -> ! {
            ubos_metrics::metrics::run_ingest(&METRICS_STATICS).await
        }

        /// Bind the HTTP and metrics listeners, and spawn a worker pool
        /// for each. Call once, after the stack has an address.
        #[allow(clippy::items_after_statements)]
        async fn serve_all(
            spawner: embassy_executor::Spawner,
            stack: embassy_net::Stack<'static>,
            handles: $crate::Handles,
            board_name: &'static str,
        ) {
            // Bound to names of their own so what reaches the task
            // attribute is an identifier.
            const HTTP_POOL: usize = $http_pool_sizes.http;
            const METRICS_POOL: usize = $http_pool_sizes.metrics;

            type HttpService =
                $crate::PrefixRouter<ubos_api::RestApiService, ubos_webui::DashboardService>;

            /// The exposition handler, sized to the statics it serves.
            type MetricsService = ubos_metrics::metrics::PrometheusMetricsService<
                { $max_metrics },
                { $metrics_response_bytes },
            >;

            $crate::http_worker_task!(http_worker_task, HttpService, HTTP_POOL);
            $crate::http_worker_task!(metrics_worker_task, MetricsService, METRICS_POOL);

            // One worker task per pool slot, so each has its own static
            // `TaskStorage`. A path under the API prefix reaches the API
            // and every other path reaches the dashboard; the two route
            // tables do not overlap, because the dashboard owns `/`,
            // `/login`, `/logout` and `/logo.svg`.
            ubos_api::seed_api_credentials(handles.config).await;
            ubos_webui::seed_webui_credentials(handles.config).await;
            static HTTP_SERVICE: static_cell::StaticCell<HttpService> =
                static_cell::StaticCell::new();
            let http_service: &'static HttpService = HTTP_SERVICE.init($crate::PrefixRouter {
                prefix: ubos_api::PATH_PREFIX,
                matched: ubos_api::RestApiService::new(
                    board_name,
                    env!("BUILD_ID"),
                    $mining_info,
                    $bounds,
                    handles.thermal,
                    handles.manager,
                    handles.psu,
                    handles.cooling,
                    handles.mining,
                    handles.net,
                    handles.config,
                    handles.stratum,
                    stack,
                ),
                fallback: ubos_webui::DashboardService::new(handles.net),
            });
            $crate::serve_http_pool!(
                spawner,
                stack,
                http_worker_task,
                http_service,
                HTTP_POOL,
                $crate::http_consts::Http
            );
            spawner.spawn(::defmt::unwrap!(sync_credentials_task(handles.config)));

            static METRICS_SERVICE: static_cell::StaticCell<MetricsService> =
                static_cell::StaticCell::new();
            let metrics_service: &'static MetricsService =
                METRICS_SERVICE.init(MetricsService::new(&METRICS_STATICS, handles.net));
            $crate::serve_http_pool!(
                spawner,
                stack,
                metrics_worker_task,
                metrics_service,
                METRICS_POOL,
                ubos_metrics::consts::Metrics
            );
        }
    };
}
