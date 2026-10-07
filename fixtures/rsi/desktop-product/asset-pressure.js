const asset = window.fixtureAssetPressure, pressure = window.fixturePressure;
if (asset?.error) throw Error(asset.error);
if (pressure?.error) throw Error(pressure.error);
return asset && pressure?.pending > 0
  ? {...asset, pendingAtCompletion: asset.pending, pending: pressure.pending}
  : null;
