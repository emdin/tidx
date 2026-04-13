const state = {
  chainId: Number(window.__TIDX_DEFAULT_CHAIN_ID__) || null,
  status: null,
};

const icons = {
  account:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"></path><circle cx="12" cy="7" r="4"></circle></g></svg>',
  contract:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z"></path><path d="M14 2v5a1 1 0 0 0 1 1h5"></path></g></svg>',
  receipt:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M12 17V7m4 1h-6a2 2 0 0 0 0 4h4a2 2 0 0 1 0 4H8"></path><path d="M4 3a1 1 0 0 1 1-1a1.3 1.3 0 0 1 .7.2l.933.6a1.3 1.3 0 0 0 1.4 0l.934-.6a1.3 1.3 0 0 1 1.4 0l.933.6a1.3 1.3 0 0 0 1.4 0l.933-.6a1.3 1.3 0 0 1 1.4 0l.934.6a1.3 1.3 0 0 0 1.4 0l.933-.6A1.3 1.3 0 0 1 19 2a1 1 0 0 1 1 1v18a1 1 0 0 1-1 1a1.3 1.3 0 0 1-.7-.2l-.933-.6a1.3 1.3 0 0 0-1.4 0l-.934.6a1.3 1.3 0 0 1-1.4 0l-.933-.6a1.3 1.3 0 0 0-1.4 0l-.933.6a1.3 1.3 0 0 1-1.4 0l-.934-.6a1.3 1.3 0 0 0-1.4 0l-.933.6a1.3 1.3 0 0 1-.7.2a1 1 0 0 1-1-1z"></path></g></svg>',
  blocks:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z"></path><path d="m3.3 7l8.7 5l8.7-5M12 22V12"></path></g></svg>',
  tokens:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M13.744 17.736a6 6 0 1 1-7.48-7.48M15 6h1v4"></path><path d="m6.134 14.768l.866-.5l2 3.464"></path><circle cx="16" cy="8" r="6"></circle></g></svg>',
  arrow:
    '<svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 12h14m-7-7l7 7l-7 7"></path></svg>',
};

const elements = {
  pageRoot: document.getElementById("page-root"),
  headerSearchSlot: document.getElementById("header-search-slot"),
  latestBlockLink: document.getElementById("latest-block-link"),
  latestBlockValue: document.getElementById("latest-block-value"),
};

document.addEventListener("DOMContentLoaded", () => {
  void boot();
});

async function boot() {
  try {
    await refreshStatus();
    renderLatestBlock();
    await renderRoute();
  } catch (error) {
    renderError(error);
  }
}

async function refreshStatus() {
  const response = await fetch("/status");
  if (!response.ok) {
    throw new Error(`Failed to load status (${response.status})`);
  }

  const json = await response.json();
  const chains = Array.isArray(json.chains) ? json.chains : [];
  const matching = chains.find((chain) => Number(chain.chain_id) === Number(state.chainId));
  state.status = matching || chains[0] || null;
  if (!state.chainId && state.status) {
    state.chainId = Number(state.status.chain_id);
  }
}

function renderLatestBlock() {
  const latest = state.status?.synced_num ?? state.status?.tip_num;
  if (!latest) {
    elements.latestBlockValue.textContent = "...";
    elements.latestBlockLink.href = "/explore/blocks";
    return;
  }

  elements.latestBlockValue.textContent = formatNumber(latest);
  elements.latestBlockLink.href = `/explore/block/${latest}`;
}

function currentRoute() {
  const prefix = "/explore";
  let path = window.location.pathname.startsWith(prefix)
    ? window.location.pathname.slice(prefix.length)
    : window.location.pathname;

  if (!path || path === "/") {
    return { name: "home" };
  }

  const parts = path.split("/").filter(Boolean);

  if (parts[0] === "blocks") {
    return { name: "blocks" };
  }
  if (parts[0] === "block" && parts[1]) {
    return { name: "block", id: parts[1] };
  }
  if ((parts[0] === "receipt" || parts[0] === "tx") && parts[1]) {
    return { name: "receipt", hash: normalizeHex(parts[1]) };
  }
  if (parts[0] === "address" && parts[1]) {
    return { name: "address", address: normalizeHex(parts[1]) };
  }
  if (parts[0] === "tokens") {
    return { name: "tokens" };
  }

  return { name: "notFound" };
}

async function renderRoute() {
  const route = currentRoute();
  const isHome = route.name === "home";
  document.body.dataset.pageMode = isHome ? "home" : "content";
  elements.headerSearchSlot.innerHTML = isHome ? "" : renderSearchForm({ compact: true });
  bindSearchForms();

  switch (route.name) {
    case "home":
      await renderHomePage();
      return;
    case "blocks":
      await renderBlocksPage();
      return;
    case "block":
      await renderBlockPage(route.id);
      return;
    case "receipt":
      await renderReceiptPage(route.hash);
      return;
    case "address":
      await renderAddressPage(route.address);
      return;
    case "tokens":
      renderTokensPage();
      return;
    default:
      renderNotFound();
  }
}

async function renderHomePage() {
  const recentTxs = await runQuery(`
    SELECT
      block_num,
      idx,
      encode(hash, 'hex') AS hash,
      encode("from", 'hex') AS from_addr,
      encode("to", 'hex') AS to_addr
    FROM txs
    ORDER BY block_num DESC, idx DESC
    LIMIT 5
  `);

  const recent = recentTxs[0] || {};
  const accountHref = recent.from_addr ? `/explore/address/0x${recent.from_addr}` : "/explore/blocks";
  const contractHref = recent.to_addr ? `/explore/address/0x${recent.to_addr}` : "/explore/blocks";
  const receiptHref = recent.hash ? `/explore/receipt/0x${recent.hash}` : "/explore/blocks";
  const latest = state.status?.synced_num ?? state.status?.tip_num ?? null;
  const syncText = latest
    ? `Chain ${formatNumber(state.chainId)} synced to block ${formatNumber(latest)}`
    : `Chain ${formatNumber(state.chainId || 0)}`;

  elements.pageRoot.innerHTML = `
    <section class="hero-page">
      <div class="hero-top-spacer">
        <div class="hero-stack">
          <span class="hero-line hero-line-subtle">Search</span>
          <span class="hero-line hero-line-mid">Explore</span>
          <span class="hero-line hero-line-main">Discover</span>
        </div>
      </div>

      <div class="hero-content">
        <div class="hero-search-wrap">
          ${renderSearchForm({ compact: false, autofocus: true })}
        </div>

        <div class="hero-status"><strong>Igra Explorer</strong> · ${escapeHtml(syncText)}</div>

        <section class="pill-row" aria-label="Explore shortcuts">
          ${renderPillLink("Account", accountHref, icons.account)}
          ${renderPillLink("Contract", contractHref, icons.contract)}
          ${renderPillLink("Receipt", receiptHref, icons.receipt)}
          ${renderPillLink("Blocks", "/explore/blocks", icons.blocks)}
          ${renderPillLink("Tokens", "/explore/tokens", icons.tokens)}
        </section>
      </div>
    </section>
  `;

  bindSearchForms();
}

async function renderBlocksPage() {
  const blocks = await runQuery(`
    SELECT
      num,
      encode(hash, 'hex') AS hash,
      timestamp,
      gas_used,
      gas_limit
    FROM blocks
    ORDER BY num DESC
    LIMIT 25
  `);

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <span class="status-pill">Live</span>
          <span class="muted-badge mono">Chain ${formatNumber(state.chainId || 0)}</span>
        </div>
        <div>
          <h1 class="page-heading">Latest blocks</h1>
          <p class="page-subheading">Tempo-style explorer shell on top of your local Igra indexer.</p>
        </div>
      </header>

      <section class="panel-card">
        <div class="panel-header">
          <div>
            <div class="panel-title">Blocks</div>
            <div class="panel-subtitle">Most recent blocks indexed by your local tidx instance</div>
          </div>
          <div class="section-actions">
            <a class="ghost-link" href="/explore">${icons.arrow} Search</a>
          </div>
        </div>
        <div class="panel-body table-wrap">
          ${renderBlocksTable(blocks)}
        </div>
      </section>
    </section>
  `;
}

async function renderBlockPage(blockId) {
  const blockNum = Number(blockId);
  if (!Number.isFinite(blockNum)) {
    renderNotFound("Invalid block number.");
    return;
  }

  const [blocks, txs, counts] = await Promise.all([
    runQuery(`
      SELECT
        num,
        encode(hash, 'hex') AS hash,
        encode(parent_hash, 'hex') AS parent_hash,
        timestamp,
        gas_used,
        gas_limit,
        encode(miner, 'hex') AS miner
      FROM blocks
      WHERE num = ${blockNum}
      LIMIT 1
    `),
    runQuery(`
      SELECT
        block_num,
        idx,
        encode(hash, 'hex') AS hash,
        encode("from", 'hex') AS from_addr,
        encode("to", 'hex') AS to_addr,
        gas_used,
        value
      FROM txs
      WHERE block_num = ${blockNum}
      ORDER BY idx ASC
      LIMIT 100
    `),
    runQuery(`
      SELECT
        (SELECT COUNT(*) FROM txs WHERE block_num = ${blockNum}) AS tx_count,
        (SELECT COUNT(*) FROM logs WHERE block_num = ${blockNum}) AS log_count,
        (SELECT COUNT(*) FROM receipts WHERE block_num = ${blockNum}) AS receipt_count
    `),
  ]);

  const block = blocks[0];
  if (!block) {
    renderNotFound(`Block ${escapeHtml(blockId)} was not found in the local index.`);
    return;
  }

  const stats = counts[0] || {};

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <a class="muted-badge" href="/explore/blocks">${icons.blocks} Back to blocks</a>
        </div>
        <div>
          <h1 class="page-heading">Block ${formatNumber(block.num)}</h1>
          <p class="page-subheading">Timestamp ${escapeHtml(formatTimestamp(block.timestamp))}</p>
        </div>
      </header>

      <div class="page-grid">
        <aside class="summary-card">
          ${renderSummaryRow("Number", formatNumber(block.num))}
          ${renderSummaryRow("Hash", linkToReceiptLike(`/explore/block/${block.num}`, with0x(block.hash), true))}
          ${renderSummaryRow("Parent", `<a href="/explore/block/${block.num - 1}">${escapeHtml(with0x(block.parent_hash))}</a>`)}
          ${renderSummaryRow("Miner", `<a href="/explore/address/${with0x(block.miner)}">${escapeHtml(with0x(block.miner))}</a>`)}
          ${renderSummaryRow("Gas used", formatNumber(block.gas_used))}
          ${renderSummaryRow("Gas limit", formatNumber(block.gas_limit))}
          ${renderSummaryRow("Transactions", formatNumber(stats.tx_count || 0))}
          ${renderSummaryRow("Logs", formatNumber(stats.log_count || 0))}
        </aside>

        <section class="panel-card">
          <div class="panel-header">
            <div>
              <div class="panel-title">Transactions</div>
              <div class="panel-subtitle">${formatNumber(stats.tx_count || 0)} transaction(s) in this block</div>
            </div>
          </div>
          <div class="panel-body table-wrap">
            ${renderBlockTransactionsTable(txs)}
          </div>
        </section>
      </div>
    </section>
  `;
}

async function renderReceiptPage(hash) {
  if (!isHex(hash, 64)) {
    renderNotFound("Invalid transaction hash.");
    return;
  }

  const body = hexBody(hash);
  const [txRows, logRows] = await Promise.all([
    runQuery(`
      SELECT
        txs.block_num,
        txs.idx,
        encode(txs.hash, 'hex') AS hash,
        encode(txs."from", 'hex') AS from_addr,
        encode(txs."to", 'hex') AS to_addr,
        txs.value,
        txs.gas_limit,
        txs.gas_used,
        txs.nonce,
        txs.type,
        receipts.status,
        receipts.effective_gas_price,
        receipts.cumulative_gas_used,
        encode(receipts.contract_address, 'hex') AS contract_address
      FROM txs
      LEFT JOIN receipts ON receipts.tx_hash = txs.hash
      WHERE txs.hash = decode('${body}', 'hex')
      LIMIT 1
    `),
    runQuery(`
      SELECT
        log_idx,
        encode(address, 'hex') AS address,
        encode(topic0, 'hex') AS topic0,
        encode(topic1, 'hex') AS topic1,
        encode(topic2, 'hex') AS topic2,
        encode(topic3, 'hex') AS topic3
      FROM logs
      WHERE tx_hash = decode('${body}', 'hex')
      ORDER BY log_idx ASC
      LIMIT 100
    `),
  ]);

  const tx = txRows[0];
  if (!tx) {
    renderNotFound(`Receipt ${escapeHtml(hash)} was not found in the local index.`);
    return;
  }

  const statusText = Number(tx.status) === 1 ? "Success" : tx.status === null ? "Pending" : "Failed";

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <a class="muted-badge" href="/explore/blocks">${icons.blocks} Blocks</a>
          <span class="muted-badge mono">${escapeHtml(shortHex(hash))}</span>
        </div>
        <div>
          <h1 class="page-heading">Receipt</h1>
          <p class="page-subheading mono">${escapeHtml(hash)}</p>
        </div>
      </header>

      <div class="page-grid">
        <aside class="summary-card">
          ${renderSummaryRow("Status", escapeHtml(statusText))}
          ${renderSummaryRow("Block", `<a href="/explore/block/${tx.block_num}">${formatNumber(tx.block_num)}</a>`)}
          ${renderSummaryRow("Index", formatNumber(tx.idx))}
          ${renderSummaryRow("From", `<a href="/explore/address/${with0x(tx.from_addr)}">${escapeHtml(with0x(tx.from_addr))}</a>`)}
          ${renderSummaryRow("To", tx.to_addr ? `<a href="/explore/address/${with0x(tx.to_addr)}">${escapeHtml(with0x(tx.to_addr))}</a>` : '<span class="text-secondary">Contract creation</span>')}
          ${renderSummaryRow("Contract", tx.contract_address ? `<a href="/explore/address/${with0x(tx.contract_address)}">${escapeHtml(with0x(tx.contract_address))}</a>` : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Nonce", formatNumber(tx.nonce))}
          ${renderSummaryRow("Type", formatNumber(tx.type))}
          ${renderSummaryRow("Gas used", formatNumber(tx.gas_used))}
          ${renderSummaryRow("Gas limit", formatNumber(tx.gas_limit))}
          ${renderSummaryRow("Value", escapeHtml(tx.value || "0"))}
        </aside>

        <section class="panel-card">
          <div class="panel-header">
            <div>
              <div class="panel-title">Logs</div>
              <div class="panel-subtitle">${formatNumber(logRows.length)} log(s)</div>
            </div>
          </div>
          <div class="panel-body table-wrap">
            ${renderLogsTable(logRows)}
          </div>
        </section>
      </div>
    </section>
  `;
}

async function renderAddressPage(address) {
  if (!isHex(address, 40)) {
    renderNotFound("Invalid address.");
    return;
  }

  const body = hexBody(address);
  const [summaryRows, txRows] = await Promise.all([
    runQuery(`
      SELECT
        COUNT(*) AS tx_count,
        MIN(block_num) AS first_seen_block,
        MAX(block_num) AS last_seen_block
      FROM txs
      WHERE "from" = decode('${body}', 'hex') OR "to" = decode('${body}', 'hex')
    `),
    runQuery(`
      SELECT
        block_num,
        idx,
        encode(hash, 'hex') AS hash,
        encode("from", 'hex') AS from_addr,
        encode("to", 'hex') AS to_addr,
        value,
        gas_used
      FROM txs
      WHERE "from" = decode('${body}', 'hex') OR "to" = decode('${body}', 'hex')
      ORDER BY block_num DESC, idx DESC
      LIMIT 50
    `),
  ]);

  const summary = summaryRows[0] || {};

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <a class="muted-badge" href="/explore">${icons.arrow} Search</a>
          <span class="muted-badge mono">${escapeHtml(shortHex(address))}</span>
        </div>
        <div>
          <h1 class="page-heading">Address</h1>
          <p class="page-subheading mono">${escapeHtml(address)}</p>
        </div>
      </header>

      <div class="page-grid">
        <aside class="summary-card">
          ${renderSummaryRow("Address", escapeHtml(address))}
          ${renderSummaryRow("Transactions", formatNumber(summary.tx_count || 0))}
          ${renderSummaryRow("First seen", summary.first_seen_block ? `<a href="/explore/block/${summary.first_seen_block}">${formatNumber(summary.first_seen_block)}</a>` : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Last seen", summary.last_seen_block ? `<a href="/explore/block/${summary.last_seen_block}">${formatNumber(summary.last_seen_block)}</a>` : '<span class="text-secondary">-</span>')}
        </aside>

        <section class="panel-card">
          <div class="panel-header">
            <div>
              <div class="panel-title">Transactions</div>
              <div class="panel-subtitle">${formatNumber(txRows.length)} recent transaction(s)</div>
            </div>
          </div>
          <div class="panel-body table-wrap">
            ${renderAddressTransactionsTable(txRows, address)}
          </div>
        </section>
      </div>
    </section>
  `;
}

function renderTokensPage() {
  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div>
          <h1 class="page-heading">Tokens</h1>
          <p class="page-subheading">This Tempo-style shell is live; token metadata pages are the next layer.</p>
        </div>
      </header>
      <section class="not-found-card">
        <div class="not-found-copy">
          Token discovery is not wired yet because tidx currently indexes generic EVM chain data,
          not token metadata or verified contract catalogs. Blocks, receipts, and address pages are ready now.
        </div>
      </section>
    </section>
  `;
}

function renderNotFound(message = "This explorer route is not available yet.") {
  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <section class="not-found-card">
        <div class="not-found-copy">${escapeHtml(message)}</div>
      </section>
    </section>
  `;
}

function renderError(error) {
  const message = error instanceof Error ? error.message : String(error);
  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <section class="not-found-card">
        <div class="not-found-copy">Explorer failed to load: ${escapeHtml(message)}</div>
      </section>
    </section>
  `;
}

function renderSearchForm({ compact, autofocus = false }) {
  const inputClass = compact ? "search-input search-input-small" : "search-input search-input-large";
  const autoFocusAttr = autofocus ? "autofocus" : "";
  return `
    <form class="search-form" data-search-form>
      <input
        ${autoFocusAttr}
        class="${inputClass}"
        type="text"
        name="explore-query"
        placeholder="Search by Address / Tx Hash / Block / Token"
        spellcheck="false"
        autocomplete="off"
      />
      <button class="search-submit" type="submit" aria-label="Search">
        ${icons.arrow}
      </button>
    </form>
  `;
}

function bindSearchForms() {
  document.querySelectorAll("[data-search-form]").forEach((form) => {
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const input = form.querySelector("input[name='explore-query']");
      const value = input?.value?.trim() || "";
      const target = resolveSearchTarget(value);
      if (!target) {
        input?.focus();
        return;
      }
      window.location.href = target;
    });
  });
}

function resolveSearchTarget(value) {
  if (!value) {
    return null;
  }

  if (/^\d+$/.test(value)) {
    return `/explore/block/${value}`;
  }

  const normalized = normalizeHex(value);
  if (isHex(normalized, 64)) {
    return `/explore/receipt/${normalized}`;
  }
  if (isHex(normalized, 40)) {
    return `/explore/address/${normalized}`;
  }

  return "/explore/tokens";
}

function renderPillLink(label, href, icon) {
  return `
    <a class="pill-link" href="${href}">
      <span class="pill-icon">${icon}</span>
      <span>${escapeHtml(label)}</span>
    </a>
  `;
}

function renderBlocksTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No blocks indexed yet.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td class="mono"><a href="/explore/block/${row.num}">${formatNumber(row.num)}</a></td>
          <td>${escapeHtml(formatRelativeTime(row.timestamp))}</td>
          <td class="mono"><a href="/explore/block/${row.num}">${escapeHtml(shortHex(with0x(row.hash), 12))}</a></td>
          <td class="align-right mono">${formatNumber(row.gas_used)}</td>
          <td class="align-right mono">${formatNumber(row.gas_limit)}</td>
        </tr>
      `,
    )
    .join("");

  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>Block</th>
          <th>Time</th>
          <th>Hash</th>
          <th class="align-right">Gas used</th>
          <th class="align-right">Gas limit</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function renderBlockTransactionsTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No transactions found in this block.</div>';
  }

  const body = rows
    .map((row) => {
      const toAddress = row.to_addr ? with0x(row.to_addr) : null;
      return `
        <tr>
          <td class="mono"><a href="/explore/receipt/${with0x(row.hash)}">${escapeHtml(shortHex(with0x(row.hash), 10))}</a></td>
          <td class="mono"><a href="/explore/address/${with0x(row.from_addr)}">${escapeHtml(shortHex(with0x(row.from_addr), 9))}</a></td>
          <td class="mono">${toAddress ? `<a href="/explore/address/${toAddress}">${escapeHtml(shortHex(toAddress, 9))}</a>` : '<span class="text-secondary">create</span>'}</td>
          <td class="align-right mono">${formatNumber(row.gas_used)}</td>
          <td class="align-right mono">${escapeHtml(row.value || "0")}</td>
        </tr>
      `;
    })
    .join("");

  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>Hash</th>
          <th>From</th>
          <th>To</th>
          <th class="align-right">Gas used</th>
          <th class="align-right">Value</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function renderAddressTransactionsTable(rows, address) {
  if (!rows.length) {
    return '<div class="empty-state">No address activity found.</div>';
  }

  const normalized = normalizeHex(address);
  const body = rows
    .map((row) => {
      const from = with0x(row.from_addr);
      const to = row.to_addr ? with0x(row.to_addr) : null;
      const direction = normalizeHex(from) === normalized ? "OUT" : "IN";
      const counterparty = direction === "OUT" ? to : from;

      return `
        <tr>
          <td class="mono"><a href="/explore/block/${row.block_num}">${formatNumber(row.block_num)}</a></td>
          <td><span class="${direction === "IN" ? "text-accent" : "text-secondary"}">${direction}</span></td>
          <td class="mono"><a href="/explore/receipt/${with0x(row.hash)}">${escapeHtml(shortHex(with0x(row.hash), 10))}</a></td>
          <td class="mono">${counterparty ? `<a href="/explore/address/${counterparty}">${escapeHtml(shortHex(counterparty, 9))}</a>` : '<span class="text-secondary">-</span>'}</td>
          <td class="align-right mono">${escapeHtml(row.value || "0")}</td>
        </tr>
      `;
    })
    .join("");

  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>Block</th>
          <th>Dir</th>
          <th>Hash</th>
          <th>Counterparty</th>
          <th class="align-right">Value</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function renderLogsTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No logs recorded for this receipt.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td class="mono">${formatNumber(row.log_idx)}</td>
          <td class="mono"><a href="/explore/address/${with0x(row.address)}">${escapeHtml(shortHex(with0x(row.address), 10))}</a></td>
          <td class="mono">${escapeHtml(shortHex(with0x(row.topic0), 12))}</td>
          <td class="mono">${row.topic1 ? escapeHtml(shortHex(with0x(row.topic1), 12)) : '<span class="text-secondary">-</span>'}</td>
          <td class="mono">${row.topic2 ? escapeHtml(shortHex(with0x(row.topic2), 12)) : '<span class="text-secondary">-</span>'}</td>
        </tr>
      `,
    )
    .join("");

  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>Index</th>
          <th>Address</th>
          <th>Topic0</th>
          <th>Topic1</th>
          <th>Topic2</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function renderSummaryRow(label, value) {
  return `
    <div class="summary-row">
      <span class="summary-label">${escapeHtml(label)}</span>
      <span class="summary-value mono">${value}</span>
    </div>
  `;
}

function linkToReceiptLike(href, value, short = false) {
  const display = short ? shortHex(value, 12) : value;
  return `<a href="${href}">${escapeHtml(display)}</a>`;
}

async function runQuery(sql) {
  if (!state.chainId) {
    throw new Error("No chain ID is configured.");
  }

  const url = `/query?chainId=${encodeURIComponent(state.chainId)}&sql=${encodeURIComponent(compactSql(sql))}`;
  const response = await fetch(url);
  const payload = await safeJson(response);

  if (!response.ok || !payload?.ok) {
    const error = payload?.error || payload?.message || `Query failed (${response.status})`;
    throw new Error(error);
  }

  return rowsToObjects(payload.columns || [], payload.rows || []);
}

async function safeJson(response) {
  try {
    return await response.json();
  } catch (_error) {
    return null;
  }
}

function rowsToObjects(columns, rows) {
  return rows.map((row) => {
    const record = {};
    columns.forEach((column, index) => {
      record[column] = row[index];
    });
    return record;
  });
}

function compactSql(sql) {
  return sql.replace(/\s+/g, " ").trim();
}

function normalizeHex(value) {
  const trimmed = String(value || "").trim();
  if (!trimmed) {
    return "";
  }
  return trimmed.startsWith("0x") ? trimmed.toLowerCase() : `0x${trimmed.toLowerCase()}`;
}

function isHex(value, bytes) {
  const normalized = normalizeHex(value);
  return new RegExp(`^0x[0-9a-f]{${bytes}}$`).test(normalized);
}

function hexBody(value) {
  return normalizeHex(value).slice(2);
}

function with0x(value) {
  if (!value) {
    return "";
  }
  return value.startsWith("0x") ? value.toLowerCase() : `0x${String(value).toLowerCase()}`;
}

function shortHex(value, visible = 8) {
  const normalized = with0x(value);
  if (!normalized) {
    return "-";
  }
  if (normalized.length <= visible * 2 + 2) {
    return normalized;
  }
  return `${normalized.slice(0, visible + 2)}...${normalized.slice(-visible)}`;
}

function formatNumber(value) {
  const num = Number(value);
  if (!Number.isFinite(num)) {
    return String(value ?? "-");
  }
  return new Intl.NumberFormat("en-US").format(num);
}

function formatTimestamp(value) {
  if (!value) {
    return "-";
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return String(value);
  }
  return date.toLocaleString("en-US", {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    timeZoneName: "short",
  });
}

function formatRelativeTime(value) {
  if (!value) {
    return "-";
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return String(value);
  }

  const deltaMs = date.getTime() - Date.now();
  const abs = Math.abs(deltaMs);
  const minute = 60_000;
  const hour = 60 * minute;
  const day = 24 * hour;
  const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

  if (abs < hour) {
    return rtf.format(Math.round(deltaMs / minute), "minute");
  }
  if (abs < day) {
    return rtf.format(Math.round(deltaMs / hour), "hour");
  }
  return rtf.format(Math.round(deltaMs / day), "day");
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}
