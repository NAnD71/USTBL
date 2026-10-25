import { Button, ButtonProps, Tooltip } from "@chakra-ui/react";
import { ComponentProps } from "react";
import { useTranslation } from "react-i18next";
import { CommonIconButton } from "@/components/common/common-icon-button";
import { InstanceSummary } from "@/models/instance/misc";
import { isInstallingInstance } from "@/utils/installing-instance";

interface InstanceLaunchProps {
  instance?: InstanceSummary;
}

const installingReasonKey =
  "Services.launch.selectSuitableJRE.error.description.LAUNCH_INSTANCE_INSTALLING";

export const InstanceLaunchButton = ({
  instance,
  ...props
}: ButtonProps & InstanceLaunchProps) => {
  const { t } = useTranslation();
  const installing = isInstallingInstance(instance);
  return (
    <Tooltip
      label={t(installingReasonKey)}
      isDisabled={!installing}
      shouldWrapChildren
    >
      <Button {...props} isDisabled={props.isDisabled || installing} />
    </Tooltip>
  );
};

export const InstanceLaunchIconButton = ({
  instance,
  ...props
}: ComponentProps<typeof CommonIconButton> & InstanceLaunchProps) => {
  const { t } = useTranslation();
  const installing = isInstallingInstance(instance);
  return (
    <Tooltip
      label={t(installingReasonKey)}
      isDisabled={!installing}
      shouldWrapChildren
    >
      <CommonIconButton
        {...props}
        withTooltip={!installing && props.withTooltip !== false}
        isDisabled={props.isDisabled || installing}
      />
    </Tooltip>
  );
};
