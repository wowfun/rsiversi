// Every registered owner is settled even when another owner's cleanup fails.
export async function cleanupAll(...actions) {
  const results = await Promise.allSettled(actions.map(action => Promise.resolve().then(action)));
  const errors = results.filter(result => result.status === 'rejected').map(result => result.reason);
  if (errors.length) throw new AggregateError(errors, 'Fixture cleanup failed');
}

// Final evidence scanning must follow every writer's settlement, including failures.
export async function cleanupFinally(finalize, ...actions) {
  let failure;
  try { await cleanupAll(...actions); } catch (error) { failure = error; }
  await cleanupAll(() => { if (failure) throw failure; }, finalize);
}

// A cleanup failure supplements the probe failure rather than replacing it.
export async function finishFixture(primary, ...actions) {
  try { await cleanupAll(...actions); }
  catch (cleanup) {
    if (primary) throw new AggregateError([primary, cleanup], 'Fixture and cleanup failed', {cause: primary});
    throw cleanup;
  }
}
