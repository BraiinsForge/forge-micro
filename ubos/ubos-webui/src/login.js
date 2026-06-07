// Surface the "Invalid username or password" banner when the server
// bounced us back here with ?err=1. The login endpoint sets that
// query parameter on auth failure and redirects so the password
// never lands in the browser history as a POST body.
const params = new URLSearchParams(location.search);
if (params.get('err')) {
  document.getElementById('err').classList.add('show');
}
