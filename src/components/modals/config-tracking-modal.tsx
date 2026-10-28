import {
  Alert,
  AlertIcon,
  Badge,
  Box,
  Button,
  Collapse,
  Divider,
  HStack,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  ModalProps,
  SimpleGrid,
  Spinner,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { DiffFile, DiffHunks } from "@/components/common/diff-view";
import {
  ChangeKind,
  ConfigTrackingTimeline,
  EntryChanges,
  FileChange,
  FileDiffBody,
  FileEntry,
  TimelineEntry,
} from "@/models/config-tracking";
import { ConfigTrackingService } from "@/services/config-tracking";
import {
  checkpointLabelKey,
  createLatestRequestGate,
  formatChangeCounts,
  formatHistoryBytes,
  semanticLabelKey,
  totalHistoryBytes,
} from "@/utils/config-tracking";
import { ISOToDatetime } from "@/utils/datetime";

interface ConfigTrackingModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
}

const KIND_COLORS: Record<ChangeKind, string> = {
  added: "green",
  modified: "orange",
  deleted: "red",
  uncertain: "gray",
};

const ConfigTrackingModal: React.FC<ConfigTrackingModalProps> = ({
  instanceId,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const [data, setData] = useState<ConfigTrackingTimeline | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [scanError, setScanError] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [diagnosticsOpen, setDiagnosticsOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [entryChanges, setEntryChanges] = useState<EntryChanges | null>(null);
  const [entryError, setEntryError] = useState<string | null>(null);
  const timelineRequests = useRef(createLatestRequestGate());
  const entryRequests = useRef(createLatestRequestGate());
  const scanRequests = useRef(createLatestRequestGate());

  const loadTimeline = useCallback(async () => {
    const requestId = timelineRequests.current.begin();
    entryRequests.current.begin();
    setData(null);
    setLoadError(null);
    setSelectedId(null);
    setEntryChanges(null);
    setEntryError(null);
    const response = await ConfigTrackingService.getTimeline(instanceId);
    if (!timelineRequests.current.isCurrent(requestId)) return;
    if (response.status === "success") {
      setData(response.data);
    } else {
      setLoadError(response.details || response.message);
    }
  }, [instanceId]);

  useEffect(() => {
    const timelineRequestGate = timelineRequests.current;
    const entryRequestGate = entryRequests.current;
    const scanRequestGate = scanRequests.current;
    void loadTimeline();
    return () => {
      timelineRequestGate.dispose();
      entryRequestGate.dispose();
      scanRequestGate.dispose();
    };
  }, [loadTimeline]);

  const entries = useMemo(
    () => [...(data?.timeline.entries ?? [])].sort((a, b) => b.id - a.id),
    [data]
  );

  const selectEntry = useCallback(
    async (entry: TimelineEntry) => {
      const requestId = entryRequests.current.begin();
      setSelectedId(entry.id);
      setEntryChanges(null);
      setEntryError(null);
      const response = await ConfigTrackingService.getEntryChanges(
        instanceId,
        entry.id
      );
      if (!entryRequests.current.isCurrent(requestId)) return;
      if (response.status === "success") {
        setEntryChanges(response.data);
      } else {
        setEntryError(response.details || response.message);
      }
    },
    [instanceId]
  );

  const createCheckpoint = async () => {
    const requestId = scanRequests.current.begin();
    setScanning(true);
    setScanError(null);
    const response = await ConfigTrackingService.scan(instanceId);
    if (!scanRequests.current.isCurrent(requestId)) return;
    if (response.status === "success") {
      setScanning(false);
      await loadTimeline();
    } else {
      setScanError(response.details || response.message);
      setScanning(false);
    }
  };

  const renderFileMetadata = (entry: FileEntry | null, label: string) => (
    <Text fontSize="xs" className="secondary-text">
      {t("ConfigTrackingModal.binary.metadata", {
        label,
        sha1: entry?.sha1 ?? t("ConfigTrackingModal.binary.missing"),
        size: formatHistoryBytes(entry?.size ?? 0),
      })}
    </Text>
  );

  const renderBody = (change: FileChange, body: FileDiffBody) => {
    if (body.type === "unavailable") {
      return (
        <VStack align="stretch" spacing={1} px={3} py={2}>
          {change.kind === "uncertain" && (
            <Text fontSize="xs" className="secondary-text">
              {t("ConfigTrackingModal.uncertain")}
            </Text>
          )}
          <Text fontSize="xs" className="secondary-text">
            {t("ConfigTrackingModal.unavailable", { reason: body.reason })}
          </Text>
        </VStack>
      );
    }
    if (change.kind === "uncertain") {
      return (
        <Text fontSize="xs" className="secondary-text" px={3} py={2}>
          {t("ConfigTrackingModal.uncertain")}
        </Text>
      );
    }
    if (body.type === "binary") {
      return (
        <VStack align="stretch" spacing={1} px={3} py={2}>
          {renderFileMetadata(change.old, t("ConfigTrackingModal.binary.old"))}
          {renderFileMetadata(change.new, t("ConfigTrackingModal.binary.new"))}
        </VStack>
      );
    }
    return (
      <VStack align="stretch" spacing={2}>
        {body.normalizedEqual && (
          <Text fontSize="xs" className="secondary-text" px={3} pt={2}>
            {t("ConfigTrackingModal.normalizedEqual")}
          </Text>
        )}
        {body.hunks.length > 0 && <DiffHunks hunks={body.hunks} />}
      </VStack>
    );
  };

  const renderChanges = () => {
    if (selectedId === null) {
      return (
        <Text fontSize="sm" className="secondary-text">
          {t("ConfigTrackingModal.selectEntry")}
        </Text>
      );
    }
    if (entryError) {
      return (
        <Alert status="error" borderRadius="md">
          <AlertIcon />
          <Box>
            <Text fontWeight="semibold">
              {t("ConfigTrackingModal.changesFailed")}
            </Text>
            <Text fontSize="sm">{entryError}</Text>
          </Box>
        </Alert>
      );
    }
    if (!entryChanges) {
      return (
        <VStack py={4} spacing={2}>
          <Spinner size="sm" />
          <Text fontSize="xs" className="secondary-text">
            {t("ConfigTrackingModal.loadingChanges")}
          </Text>
        </VStack>
      );
    }
    if (entryChanges.files.length === 0) {
      return (
        <Text fontSize="sm" className="secondary-text">
          {t("ConfigTrackingModal.noFiles")}
        </Text>
      );
    }
    return (
      <VStack align="stretch" spacing={3}>
        {entryChanges.files.map((change) => (
          <DiffFile
            key={change.path}
            path={change.path}
            defaultOpen={entryChanges.files.length <= 5}
            tags={
              <Badge colorScheme={KIND_COLORS[change.kind]} flexShrink={0}>
                {t(`ConfigTrackingModal.fileKind.${change.kind}`)}
              </Badge>
            }
          >
            {renderBody(change, change.body)}
          </DiffFile>
        ))}
      </VStack>
    );
  };

  return (
    <Modal
      scrollBehavior="inside"
      size={{ base: "2xl", lg: "6xl" }}
      autoFocus={false}
      {...modalProps}
    >
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>{t("ConfigTrackingModal.title")}</ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          {loadError ? (
            <Alert status="error" borderRadius="md">
              <AlertIcon />
              <Box>
                <Text fontWeight="semibold">
                  {t("ConfigTrackingModal.loadFailed")}
                </Text>
                <Text fontSize="sm">{loadError}</Text>
              </Box>
            </Alert>
          ) : !data ? (
            <VStack py={8} spacing={2}>
              <Spinner size="sm" />
              <Text fontSize="xs" className="secondary-text">
                {t("ConfigTrackingModal.loading")}
              </Text>
            </VStack>
          ) : (
            <VStack align="stretch" spacing={4}>
              <Box>
                <Text fontSize="xs" className="secondary-text">
                  {t("ConfigTrackingModal.gameRoot")}
                </Text>
                <Text fontSize="sm" wordBreak="break-all">
                  {data.gameRoot}
                </Text>
              </Box>
              {data.sharedRoot && (
                <Alert status="warning" borderRadius="md">
                  <AlertIcon />
                  <Text fontSize="sm">
                    {t("ConfigTrackingModal.sharedRoot")}
                  </Text>
                </Alert>
              )}
              <SimpleGrid columns={{ base: 1, sm: 2 }} spacing={2}>
                <Text fontSize="sm">
                  {t("ConfigTrackingModal.stats.entries", {
                    count: data.timeline.stats.entries,
                  })}
                </Text>
                <Text fontSize="sm">
                  {t("ConfigTrackingModal.stats.storage", {
                    size: formatHistoryBytes(
                      totalHistoryBytes(
                        data.timeline.stats.logBytes,
                        data.timeline.stats.objectBytes
                      )
                    ),
                  })}
                </Text>
              </SimpleGrid>
              {data.timeline.diagnostics.length > 0 && (
                <Box>
                  <Button
                    size="sm"
                    variant="link"
                    onClick={() => setDiagnosticsOpen((value) => !value)}
                    aria-expanded={diagnosticsOpen}
                    aria-controls="config-tracking-diagnostics"
                  >
                    {t("ConfigTrackingModal.diagnostics", {
                      count: data.timeline.diagnostics.length,
                    })}
                  </Button>
                  <Collapse
                    id="config-tracking-diagnostics"
                    in={diagnosticsOpen}
                    animateOpacity={false}
                  >
                    <VStack align="stretch" mt={2} spacing={1}>
                      {data.timeline.diagnostics.map((diagnostic, index) => (
                        <Text key={index} fontSize="xs" wordBreak="break-word">
                          {diagnostic}
                        </Text>
                      ))}
                    </VStack>
                  </Collapse>
                </Box>
              )}
              <Text fontSize="sm" className="secondary-text">
                {t("ConfigTrackingModal.note")}
              </Text>
              {scanError && (
                <Alert status="error" borderRadius="md">
                  <AlertIcon />
                  <Box>
                    <Text fontWeight="semibold">
                      {t("ConfigTrackingModal.scanFailed")}
                    </Text>
                    <Text fontSize="sm">{scanError}</Text>
                  </Box>
                </Alert>
              )}
              {entries.length === 0 ? (
                <Text className="secondary-text">
                  {t("ConfigTrackingModal.empty")}
                </Text>
              ) : (
                <VStack align="stretch" spacing={2}>
                  {entries.map((entry) => {
                    const counts = entry.changes
                      ? formatChangeCounts(entry.changes)
                      : null;
                    const selected = selectedId === entry.id;
                    return (
                      <Box
                        key={entry.id}
                        as="button"
                        type="button"
                        textAlign="left"
                        borderWidth="1px"
                        borderRadius="md"
                        borderColor={selected ? "blue.400" : undefined}
                        bg={selected ? "blackAlpha.50" : undefined}
                        p={3}
                        aria-pressed={selected}
                        onClick={() => void selectEntry(entry)}
                      >
                        <VStack align="stretch" spacing={1.5}>
                          <HStack wrap="wrap" spacing={2}>
                            <Text fontSize="xs" className="secondary-text">
                              {ISOToDatetime(entry.at)}
                            </Text>
                            <Badge>
                              {t(
                                `ConfigTrackingModal.${checkpointLabelKey(entry.checkpoint)}`
                              )}
                            </Badge>
                            <Badge colorScheme="purple">
                              {t(
                                `ConfigTrackingModal.${semanticLabelKey(entry.semantic)}`
                              )}
                            </Badge>
                          </HStack>
                          <HStack wrap="wrap" spacing={3} fontSize="xs">
                            {counts ? (
                              <>
                                <Text color="green.500">
                                  {t("ConfigTrackingModal.counts.added", {
                                    count: counts.added as unknown as number,
                                  })}
                                </Text>
                                <Text color="orange.500">
                                  {t("ConfigTrackingModal.counts.modified", {
                                    count: counts.modified as unknown as number,
                                  })}
                                </Text>
                                <Text color="red.500">
                                  {t("ConfigTrackingModal.counts.deleted", {
                                    count: counts.deleted as unknown as number,
                                  })}
                                </Text>
                                <Text className="secondary-text">
                                  {t("ConfigTrackingModal.counts.uncertain", {
                                    count:
                                      counts.uncertain as unknown as number,
                                  })}
                                </Text>
                              </>
                            ) : (
                              <Text className="secondary-text">
                                {t("ConfigTrackingModal.snapshotMissing")}
                              </Text>
                            )}
                          </HStack>
                          {entry.incomplete && (
                            <Text fontSize="xs" color="orange.500">
                              {t("ConfigTrackingModal.incomplete")}
                            </Text>
                          )}
                          {entry.checkpoint === "after_exit" && entry.exit && (
                            <Text fontSize="xs" className="secondary-text">
                              {t("ConfigTrackingModal.exitCode", {
                                code:
                                  entry.exit.code === null
                                    ? t("ConfigTrackingModal.unknown")
                                    : entry.exit.code,
                              })}
                            </Text>
                          )}
                          {data.sharedRoot && entry.instanceId && (
                            <Text fontSize="xs" className="secondary-text">
                              {t("ConfigTrackingModal.triggeredBy", {
                                instance: entry.instanceId,
                              })}
                            </Text>
                          )}
                        </VStack>
                      </Box>
                    );
                  })}
                </VStack>
              )}
              <Divider />
              <Box>
                <Text fontWeight="semibold" mb={2}>
                  {t("ConfigTrackingModal.changesTitle")}
                </Text>
                {renderChanges()}
              </Box>
            </VStack>
          )}
        </ModalBody>
        <ModalFooter>
          <Button
            colorScheme="blue"
            onClick={() => void createCheckpoint()}
            isDisabled={scanning || !data}
            leftIcon={scanning ? <Spinner size="xs" /> : undefined}
          >
            {scanning
              ? t("ConfigTrackingModal.scanning")
              : t("ConfigTrackingModal.scan")}
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default ConfigTrackingModal;
