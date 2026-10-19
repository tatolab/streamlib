// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// The preview page: plays the broadcast `/watch/<machine>/<stream>` names off
// the machine's local relay, pinned to the video track `<node>/<port>` when the
// path names one.

import * as Watch from "@moq/watch";

// An `http://` URL: the player fetches the relay's self-signed certificate
// fingerprint from this port and pins it.
const LOCAL_RELAY_LOOPBACK_LISTENER_URL = new URL("http://127.0.0.1:4443");

const WATCH_PAGE_ROUTE_PREFIX = "/watch/";

// The players' convention for a broadcast whose catalog is hang's.
const BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ = ".hang";

type WatchedAddress = {
	machine: string;
	stream: string;
	trackOfOneNodesPort: string | undefined;
};

function watchedAddressFromThePath(path: string): WatchedAddress | undefined {
	if (!path.startsWith(WATCH_PAGE_ROUTE_PREFIX)) {
		return undefined;
	}
	const segments = path
		.slice(WATCH_PAGE_ROUTE_PREFIX.length)
		.split("/")
		.filter((segment) => segment !== "")
		.map(decodeURIComponent);
	if (segments.length === 2) {
		return { machine: segments[0], stream: segments[1], trackOfOneNodesPort: undefined };
	}
	if (segments.length === 4) {
		return {
			machine: segments[0],
			stream: segments[1],
			trackOfOneNodesPort: `${segments[2]}/${segments[3]}`,
		};
	}
	return undefined;
}

function watchPagePathOf(address: WatchedAddress, trackOfOneNodesPort?: string): string {
	const named = [address.machine, address.stream, ...(trackOfOneNodesPort?.split("/") ?? [])];
	return WATCH_PAGE_ROUTE_PREFIX + named.map(encodeURIComponent).join("/");
}

function elementById<T extends HTMLElement>(id: string): T {
	const element = document.getElementById(id);
	if (!element) {
		throw new Error(`the preview page has no #${id}`);
	}
	return element as T;
}

function showTheStatus(status: string): void {
	elementById("watch-status").textContent = status;
}

function listTheVideoTracksToPickFrom(
	address: WatchedAddress,
	videoTrackNames: string[],
	playingTrackName: string | undefined,
): void {
	const videoTrackList = elementById<HTMLUListElement>("watch-video-tracks");
	videoTrackList.replaceChildren(
		...videoTrackNames.map((videoTrackName) => {
			const item = document.createElement("li");
			const link = document.createElement("a");
			link.href = watchPagePathOf(address, videoTrackName);
			link.textContent = videoTrackName;
			if (videoTrackName === playingTrackName) {
				link.setAttribute("aria-current", "true");
			}
			item.append(link);
			return item;
		}),
	);
}

function playTheWatchedAddress(address: WatchedAddress): void {
	const broadcast = `${address.machine}/${address.stream}${BROADCAST_SUFFIX_OF_A_CATALOG_THE_PLAYERS_READ}`;
	elementById("watch-address").textContent = address.trackOfOneNodesPort
		? `${broadcast} · ${address.trackOfOneNodesPort}`
		: broadcast;

	const connection = new Watch.Net.Connection({
		url: LOCAL_RELAY_LOOPBACK_LISTENER_URL,
		enabled: true,
	});
	const player = new Watch.Player({
		origin: connection.origin,
		probe: connection.probe,
		name: Watch.Net.Path.from(broadcast),
		canvas: elementById<HTMLCanvasElement>("watch-canvas"),
		target: address.trackOfOneNodesPort ? { name: address.trackOfOneNodesPort } : undefined,
	});

	const showWhatIsPlaying = () => {
		const videoTrackNames = Object.keys(player.broadcast.out.catalog.peek()?.video?.renditions ?? {});
		const playingTrackName = player.video.source.out.track.peek();
		listTheVideoTracksToPickFrom(address, videoTrackNames, playingTrackName);
		const broadcastStatus = player.broadcast.out.status.peek();
		if (broadcastStatus !== "live") {
			showTheStatus(`${broadcastStatus} — waiting for ${broadcast} at ${LOCAL_RELAY_LOOPBACK_LISTENER_URL.host}`);
		} else if (address.trackOfOneNodesPort && !videoTrackNames.includes(address.trackOfOneNodesPort)) {
			showTheStatus(`${address.trackOfOneNodesPort} is not a video track of ${broadcast}`);
		} else if (player.video.source.out.error.peek() === "unsupported") {
			showTheStatus("this browser cannot decode any video track of this broadcast");
		} else {
			showTheStatus(playingTrackName ? `playing ${playingTrackName}` : "live");
		}
	};
	player.broadcast.out.status.subscribe(showWhatIsPlaying);
	player.broadcast.out.catalog.subscribe(showWhatIsPlaying);
	player.video.source.out.track.subscribe(showWhatIsPlaying);
	player.video.source.out.error.subscribe(showWhatIsPlaying);
	showWhatIsPlaying();
}

const watchedAddress = watchedAddressFromThePath(location.pathname);
if (watchedAddress) {
	playTheWatchedAddress(watchedAddress);
} else {
	showTheStatus("open /watch/<machine>/<stream> or /watch/<machine>/<stream>/<node>/<port>");
}
