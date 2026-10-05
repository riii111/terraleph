# クラウド接続なしの実シナリオ

リポジトリルートから起動する。Python 3、Cargo、TerraformまたはOpenTofuが必要。初回はregistryへの接続でrandom providerを取得する。クラウド接続、Docker、認証情報は不要。

```sh
python3 fixtures/demo.py --list
python3 fixtures/demo.py failure
python3 fixtures/demo.py lock --tool tofu
```

| シナリオ | 状況と確認観点 |
|---|---|
| `sensitive` | dev/prodのsensitive変数・output、random_password、ネストした機微値。`P`で両環境をplanし、Compare、`v`でPlan Review、`Esc`でOverview、コピーで値の秘匿を確認する |
| `large` | 300件のupdateと1,000文字以上の属性値。スクロール、フィルタ、グループ展開、描画速度を確認する |
| `failure` | 1〜3秒後に成功する3リソースと途中で失敗するlocal-exec。applyの進捗、ログ、終了コード、端末復元を確認する |
| `lock` | レビュー後のapply直前に別プロセスが実際のlocal state lockを取得する。取得エラーの表示と操作継続を確認する |
| `stale` | レビュー後のapply直前に別applyでstateを更新する。saved planのstaleエラーと、暗黙の再planがないことを確認する |
| `diagnostics` | `P`で両環境をplanする。warning環境のcheck警告、error環境のprecondition失敗とエラー詳細への導線を確認する |

`lock`・`stale`のapply用ラッパーは実ツールで競合を発生させ、Terralephが渡したsaved planをそのままapplyする。lock保持プロセスは競合apply終了時に停止・終了待ちする。lockはPOSIX環境で実行する。データは合成値で、plan・stateは一時ディレクトリに置き、終了時に削除する。`single`・`multi`の既存コマンドも利用できる。

非対話検証は実ツールでplanの機微値マーカー、300件の更新、成功・失敗の進捗イベント、lock競合と解放、stale拒否とstate保持、warning/errorを検証する。`--binary`を付けると各シナリオを実CLIのPTYでも実行し、表示、終了コード、画面とカーソルの復元を確認する。sensitiveの合成秘密値が端末出力に現れた場合は`OBSERVED`を出す。これは既存Rust実装の観測で、シナリオ生成の成否とは分けて報告する。

```sh
python3 fixtures/cloudless/acceptance.py --tool terraform --binary target/debug/terraleph
python3 fixtures/cloudless/acceptance.py --tool tofu --binary target/debug/terraleph
python3 fixtures/cloudless/acceptance.py lock stale --tool tofu
```

描画速度、操作感、機微値のコピー先、端末の入力復元は実端末で確認する。非対話検証はこれらの手動確認を代替しない。
